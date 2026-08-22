//! One-run worker core.  Durable persistence is deliberately injected so a
//! worker cannot bypass the lease capability while the storage boundary evolves.

use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::artifact::ArtifactScanner;
use openwork_execution::store::{CancellationEvidence, RunLease, RunQueueRepository};
use openwork_execution::{
    ApprovedMountDirectory, Artifact, DigestPinnedImageRef, Run, RunStatus, RuntimeTask,
    SandboxBackend, SandboxLimits, SandboxNetworkPolicy, SandboxResult, SandboxTermination,
    SandboxUser, SandboxWorkingDirectory, Sha256Digest, UtcTimestamp,
};
use openwork_runtime::task::{RuntimeTaskAdapter, decode_sandbox_result, into_sandbox_request};
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

/// One-time prompt material.  It is deliberately non-cloneable and its debug
/// representation never contains the prompt.
pub struct OneTimePrompt(String);

impl OneTimePrompt {
    /// Creates a prompt that can be consumed by exactly one worker call.
    #[must_use]
    pub fn new(prompt: String) -> Self {
        Self(prompt)
    }

    fn take(self) -> String {
        self.0
    }
}

impl fmt::Debug for OneTimePrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OneTimePrompt(<redacted>)")
    }
}

/// Non-sensitive task properties, supplied with the one-time prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerTaskSpec {
    pub runtime: String,
    pub prompt_sha256: Sha256Digest,
    pub working_directory: SandboxWorkingDirectory,
    pub timeout_seconds: u64,
    pub capabilities: Vec<String>,
}

/// A worker request contains no serializable prompt-bearing field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerEnvironment {
    pub image: DigestPinnedImageRef,
    pub user: SandboxUser,
    pub input_directory: ApprovedMountDirectory,
    pub output_directory: ApprovedMountDirectory,
    pub limits: SandboxLimits,
    pub network: SandboxNetworkPolicy,
    pub artifact_output_root: PathBuf,
    pub max_artifact_bytes: u64,
}

/// Decision already made by the policy/approval layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartDisposition {
    Run,
    AwaitingApproval,
}

/// Bounds database lease traffic while a blocking sandbox invocation runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisorConfig {
    pub heartbeat_interval: Duration,
    pub lease_ttl: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(30),
        }
    }
}

/// Injectable, lease-bound persistence operations required by a worker.
/// Implementors must validate the current lease token, owner, expiry and
/// expected revision in the same transaction as the state/artifact/audit write.
#[allow(clippy::missing_errors_doc)]
pub trait WorkerLeaseStore: Send + Sync {
    /// Extends the current lease while returning the latest run revision and
    /// cancellation intent. Implementations must fail expired leases closed.
    fn heartbeat(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<RunLease, OpenWorkError>;

    /// Advances a non-terminal state and writes its redacted audit event.
    fn transition(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        now: UtcTimestamp,
    ) -> Result<RunLease, OpenWorkError>;

    /// Persists all validated artifacts plus their audit event under this lease.
    fn record_artifacts(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        artifacts: Vec<Artifact>,
        now: UtcTimestamp,
    ) -> Result<(), OpenWorkError>;

    /// Appends a redacted runtime lifecycle event under the current lease.
    fn append_runtime_audit(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        event_type: openwork_execution::AuditEventType,
        now: UtcTimestamp,
    ) -> Result<(), OpenWorkError>;

    /// Writes a non-cancel terminal state and its redacted audit event.
    fn complete(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<Run, OpenWorkError>;

    /// Returns whether a valid current lease has received cancellation intent.
    fn cancel_requested(&self, lease: &RunLease, now: UtcTimestamp) -> Result<bool, OpenWorkError>;

    /// The sole persistence operation allowed to confirm cancellation.
    fn confirm_cancel(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        evidence: CancellationEvidence,
    ) -> Result<Run, OpenWorkError>;
}

impl<T: RunQueueRepository> WorkerLeaseStore for T {
    fn heartbeat(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<RunLease, OpenWorkError> {
        self.heartbeat_lease(
            lease,
            now,
            time::Duration::try_from(ttl).map_err(|_| worker_error("invalid lease ttl"))?,
        )
    }
    fn transition(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        now: UtcTimestamp,
    ) -> Result<RunLease, OpenWorkError> {
        self.transition_leased_run(lease, expected_revision, next, now)
    }
    fn record_artifacts(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        artifacts: Vec<Artifact>,
        now: UtcTimestamp,
    ) -> Result<(), OpenWorkError> {
        self.record_leased_artifacts(lease, expected_revision, artifacts, now)
    }
    fn append_runtime_audit(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        event_type: openwork_execution::AuditEventType,
        now: UtcTimestamp,
    ) -> Result<(), OpenWorkError> {
        self.append_leased_runtime_audit(lease, expected_revision, event_type, now)
            .map(|_| ())
    }
    fn complete(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<Run, OpenWorkError> {
        self.complete_leased_run(lease, expected_revision, next, reason, now)
    }
    fn cancel_requested(&self, lease: &RunLease, now: UtcTimestamp) -> Result<bool, OpenWorkError> {
        self.lease_cancel_requested(lease, now)
    }
    fn confirm_cancel(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        evidence: CancellationEvidence,
    ) -> Result<Run, OpenWorkError> {
        self.confirm_cancel(lease, now, evidence)
    }
}

/// Result of one worker invocation.  Provider output is intentionally omitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerOutcome {
    AwaitingApproval(Run),
    Completed(Run),
}

/// Minimal single-run worker. It makes no host runtime invocation: adapters
/// construct a sandbox command and prompt stdin, then the backend executes it.
pub struct SingleRunWorker<'a, S: WorkerLeaseStore, B: SandboxBackend, A: RuntimeTaskAdapter> {
    store: &'a S,
    sandbox: &'a B,
    adapter: &'a A,
    supervisor: SupervisorConfig,
}

impl<'a, S: WorkerLeaseStore, B: SandboxBackend, A: RuntimeTaskAdapter>
    SingleRunWorker<'a, S, B, A>
{
    #[must_use]
    pub fn new(store: &'a S, sandbox: &'a B, adapter: &'a A) -> Self {
        Self {
            store,
            sandbox,
            adapter,
            supervisor: SupervisorConfig::default(),
        }
    }

    /// Overrides heartbeat cadence; the interval must be positive and shorter than TTL.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid cadence or TTL.
    pub fn with_supervisor(mut self, supervisor: SupervisorConfig) -> Result<Self, OpenWorkError> {
        if supervisor.heartbeat_interval.is_zero()
            || supervisor.lease_ttl.is_zero()
            || supervisor.heartbeat_interval >= supervisor.lease_ttl
        {
            return Err(worker_error(
                "worker heartbeat interval must be positive and shorter than lease TTL",
            ));
        }
        self.supervisor = supervisor;
        Ok(self)
    }

    /// Executes exactly one claimed run. A cancellation intent before a
    /// verifiable sandbox cancellation fails the run closed instead of claiming
    /// it is cancelled.
    ///
    /// # Errors
    ///
    /// Returns an error after best-effort lease-bound failure persistence.
    pub fn execute(
        &self,
        lease: RunLease,
        task_spec: WorkerTaskSpec,
        prompt: OneTimePrompt,
        environment: WorkerEnvironment,
        disposition: StartDisposition,
    ) -> Result<WorkerOutcome, OpenWorkError> {
        let task = match build_task(&lease, task_spec, prompt) {
            Ok(task) => task,
            Err(error) => return self.fail_after_error(lease, error, UtcTimestamp::now()),
        };
        if let Err(error) = validate_environment(&lease, &environment) {
            return self.fail_after_error(lease, error, UtcTimestamp::now());
        }
        let now = UtcTimestamp::now();
        let planning = match lease.run.status {
            // The durable queue claim establishes Planning atomically.  Accept
            // that state rather than attempting an illegal self-transition.
            RunStatus::Planning => lease,
            _ => return self.fail_closed(lease, "claimed run is not in planning", now),
        };
        if self.store.cancel_requested(&planning, now)? {
            return self.fail_closed(
                planning,
                "cancellation requested before sandbox execution",
                now,
            );
        }
        if disposition == StartDisposition::AwaitingApproval {
            let awaiting = self.store.transition(
                &planning,
                planning.run.revision,
                RunStatus::AwaitingApproval,
                now,
            )?;
            return Ok(WorkerOutcome::AwaitingApproval(awaiting.run));
        }
        let running =
            self.store
                .transition(&planning, planning.run.revision, RunStatus::Running, now)?;
        if self.store.cancel_requested(&running, now)? {
            return self.fail_closed(
                running,
                "cancellation requested before sandbox execution",
                now,
            );
        }
        if let Err(error) = self.sandbox.health() {
            return self.fail_after_error(running, error, now);
        }
        let invocation = match self.adapter.prepare(&task) {
            Ok(invocation) => invocation,
            Err(error) => return self.fail_after_error(running, error, now),
        };
        // `into_sandbox_request` overwrites command stdin with adapter stdin;
        // no prompt enters argv, environment, logs, or worker output.
        let request = match into_sandbox_request(
            invocation,
            running.run.id.clone(),
            environment.image.clone(),
            environment.user,
            environment.input_directory.clone(),
            environment.output_directory.clone(),
            environment.limits,
        ) {
            Ok(request) => request.with_network(environment.network.clone()),
            Err(error) => return self.fail_after_error(running, error, now),
        };
        let (running, result) = match self.execute_with_cancellation(running.clone(), &request) {
            Ok(result) => result,
            Err(error) => return self.fail_after_error(running, error, now),
        };
        self.finish(running, result, environment)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn finish(
        &self,
        lease: RunLease,
        result: SandboxResult,
        environment: WorkerEnvironment,
    ) -> Result<WorkerOutcome, OpenWorkError> {
        let now = UtcTimestamp::now();
        if let Err(error) = result.validate() {
            return self.fail_after_error(lease, error, now);
        }
        if result.run_id != lease.run.id {
            return self.fail_closed(lease, "sandbox returned a result for another run", now);
        }
        self.audit(&lease, openwork_execution::AuditEventType::SandboxCreated)?;
        self.audit(&lease, openwork_execution::AuditEventType::RuntimeStarted)?;
        let cancel_requested = self.store.cancel_requested(&lease, now)?;
        if cancel_requested {
            if result.termination == SandboxTermination::Cancelled {
                return match CancellationEvidence::verify(&lease, &result) {
                    Ok(evidence) => self
                        .store
                        .confirm_cancel(&lease, UtcTimestamp::now(), evidence)
                        .map(WorkerOutcome::Completed),
                    Err(error) => self.fail_after_error(lease, error, UtcTimestamp::now()),
                };
            }
            return self.fail_closed(
                lease,
                "cancellation requested without proven sandbox disposal",
                UtcTimestamp::now(),
            );
        }
        let terminal = match result.termination {
            SandboxTermination::Cancelled => {
                return self.fail_closed(
                    lease,
                    "sandbox cancelled without durable cancellation intent",
                    UtcTimestamp::now(),
                );
            }
            SandboxTermination::TimedOut => RunStatus::TimedOut,
            SandboxTermination::OutOfMemory | SandboxTermination::Failed => RunStatus::Failed,
            SandboxTermination::Exited => {
                return self.finish_exited(lease, &result, &environment);
            }
        };
        self.audit(&lease, openwork_execution::AuditEventType::SandboxDestroyed)?;
        self.store
            .complete(
                &lease,
                lease.run.revision,
                terminal,
                Some("sandbox did not complete successfully"),
                UtcTimestamp::now(),
            )
            .map(WorkerOutcome::Completed)
    }

    fn finish_exited(
        &self,
        lease: RunLease,
        result: &SandboxResult,
        environment: &WorkerEnvironment,
    ) -> Result<WorkerOutcome, OpenWorkError> {
        let mut decoder = self.adapter.decoder(lease.run.id.clone());
        let events = match decode_sandbox_result(result, decoder.as_mut()) {
            Ok(events) => events,
            Err(error) => return self.fail_after_error(lease, error, UtcTimestamp::now()),
        };
        for event in &events {
            let event_type = if matches!(
                &event.payload,
                openwork_execution::RuntimeEventPayload::Completed { .. }
                    | openwork_execution::RuntimeEventPayload::Failed { .. }
                    | openwork_execution::RuntimeEventPayload::Cancelled
            ) {
                openwork_execution::AuditEventType::RuntimeCompleted
            } else {
                openwork_execution::AuditEventType::RuntimeOutput
            };
            self.audit(&lease, event_type)?;
        }
        let succeeded = events.last().is_some_and(|event| {
            matches!(
                &event.payload,
                openwork_execution::RuntimeEventPayload::Completed { exit_code: 0 }
            )
        });
        if !succeeded {
            return self.complete_failed(&lease);
        }
        let scanner = match ArtifactScanner::new(environment.max_artifact_bytes) {
            Ok(scanner) => scanner,
            Err(error) => return self.fail_after_error(lease, error, UtcTimestamp::now()),
        };
        let artifacts = match scanner.scan(
            &lease.run.id,
            &environment.artifact_output_root,
            &result.output_paths,
            UtcTimestamp::now(),
        ) {
            Ok(artifacts) => artifacts,
            Err(error) => return self.fail_after_error(lease, error, UtcTimestamp::now()),
        };
        if let Err(error) =
            self.store
                .record_artifacts(&lease, lease.run.revision, artifacts, UtcTimestamp::now())
        {
            return self.fail_after_error(lease, error, UtcTimestamp::now());
        }
        self.audit(&lease, openwork_execution::AuditEventType::SandboxDestroyed)?;
        self.store
            .complete(
                &lease,
                lease.run.revision,
                RunStatus::Succeeded,
                None,
                UtcTimestamp::now(),
            )
            .map(WorkerOutcome::Completed)
    }

    fn complete_failed(&self, lease: &RunLease) -> Result<WorkerOutcome, OpenWorkError> {
        self.audit(lease, openwork_execution::AuditEventType::SandboxDestroyed)?;
        self.store
            .complete(
                lease,
                lease.run.revision,
                RunStatus::Failed,
                Some("sandbox did not complete successfully"),
                UtcTimestamp::now(),
            )
            .map(WorkerOutcome::Completed)
    }

    /// Runs the blocking sandbox backend while retaining the lease's
    /// cancellation channel.  Cancellation is a request to the backend only;
    /// `finish` still requires returned cancellation evidence before it can
    /// persist `Cancelled`.
    fn execute_with_cancellation(
        &self,
        lease: RunLease,
        request: &openwork_execution::SandboxRequest,
    ) -> Result<(RunLease, SandboxResult), OpenWorkError> {
        std::thread::scope(|scope| {
            let (sender, receiver) = sync_channel(1);
            scope.spawn(move || {
                let _ = sender.send(self.sandbox.execute(request));
            });
            let mut cancel_sent = false;
            let mut current_lease = lease;
            let mut primary_error = None;
            loop {
                match receiver.recv_timeout(self.supervisor.heartbeat_interval) {
                    Ok(result) => {
                        if let Some(error) = primary_error {
                            return Err(error);
                        }
                        return result.map(|result| (current_lease, result));
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(worker_error("sandbox supervisor lost its execution result"));
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if primary_error.is_none() {
                    match self.store.heartbeat(
                        &current_lease,
                        UtcTimestamp::now(),
                        self.supervisor.lease_ttl,
                    ) {
                        Ok(lease) => current_lease = lease,
                        Err(error) => primary_error = Some(error),
                    }
                }
                if !cancel_sent && (primary_error.is_some() || current_lease.cancel_requested) {
                    if let Err(error) = self.sandbox.cancel(&current_lease.run.id)
                        && primary_error.is_none()
                    {
                        primary_error = Some(error);
                    }
                    cancel_sent = true;
                }
            }
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn fail_closed(
        &self,
        lease: RunLease,
        reason: &'static str,
        now: UtcTimestamp,
    ) -> Result<WorkerOutcome, OpenWorkError> {
        self.store
            .complete(
                &lease,
                lease.run.revision,
                RunStatus::Failed,
                Some(reason),
                now,
            )
            .map(WorkerOutcome::Completed)
    }

    fn audit(
        &self,
        lease: &RunLease,
        event_type: openwork_execution::AuditEventType,
    ) -> Result<(), OpenWorkError> {
        self.store
            .append_runtime_audit(lease, lease.run.revision, event_type, UtcTimestamp::now())
    }

    fn fail_after_error(
        &self,
        lease: RunLease,
        primary: OpenWorkError,
        now: UtcTimestamp,
    ) -> Result<WorkerOutcome, OpenWorkError> {
        match self.fail_closed(lease, "worker execution failed closed", now) {
            Ok(_) => Err(primary),
            Err(terminal_error) => Err(terminal_error),
        }
    }
}

fn build_task(
    lease: &RunLease,
    spec: WorkerTaskSpec,
    prompt: OneTimePrompt,
) -> Result<RuntimeTask, OpenWorkError> {
    let task = RuntimeTask {
        schema_version: openwork_execution::EXECUTION_SCHEMA_VERSION,
        run_id: lease.run.id.clone(),
        runtime: spec.runtime,
        prompt: prompt.take(),
        prompt_hash: spec.prompt_sha256,
        working_directory: spec.working_directory,
        timeout_seconds: spec.timeout_seconds,
        capabilities: spec.capabilities,
    };
    task.validate()?;
    if task.prompt_hash != lease.run.prompt_sha256 || task.runtime != lease.run.runtime {
        return Err(worker_error(
            "one-time prompt hash or runtime does not match claimed run",
        ));
    }
    Ok(task)
}

fn validate_environment(
    lease: &RunLease,
    environment: &WorkerEnvironment,
) -> Result<(), OpenWorkError> {
    if environment.input_directory.as_path() != lease.run.workspace.as_path()
        || environment.artifact_output_root.as_path() != environment.output_directory.as_path()
    {
        return Err(worker_error(
            "worker environment does not match the claimed workspace or output mount",
        ));
    }
    Ok(())
}

fn worker_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ExecutionFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwork_execution::audit::AuditAppend;
    use openwork_execution::store::{ExecutionStore, InMemoryExecutionStore, RunQueueRepository};
    use openwork_execution::{ActorId, RunId, sha256_bytes};
    use openwork_runtime::task::CodexTaskAdapter;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};
    use time::Duration;

    struct StoreAdapter(InMemoryExecutionStore);

    impl WorkerLeaseStore for StoreAdapter {
        fn heartbeat(
            &self,
            lease: &RunLease,
            now: UtcTimestamp,
            ttl: std::time::Duration,
        ) -> Result<RunLease, OpenWorkError> {
            self.0.heartbeat_lease(
                lease,
                now,
                Duration::try_from(ttl).map_err(|_| worker_error("invalid lease ttl"))?,
            )
        }
        fn transition(
            &self,
            lease: &RunLease,
            revision: u64,
            next: RunStatus,
            now: UtcTimestamp,
        ) -> Result<RunLease, OpenWorkError> {
            self.0.transition_leased_run(lease, revision, next, now)
        }
        fn record_artifacts(
            &self,
            lease: &RunLease,
            revision: u64,
            artifacts: Vec<Artifact>,
            now: UtcTimestamp,
        ) -> Result<(), OpenWorkError> {
            self.0
                .record_leased_artifacts(lease, revision, artifacts, now)
        }
        fn append_runtime_audit(
            &self,
            lease: &RunLease,
            revision: u64,
            event_type: openwork_execution::AuditEventType,
            now: UtcTimestamp,
        ) -> Result<(), OpenWorkError> {
            self.0
                .append_leased_runtime_audit(lease, revision, event_type, now)
                .map(|_| ())
        }
        fn complete(
            &self,
            lease: &RunLease,
            revision: u64,
            next: RunStatus,
            reason: Option<&str>,
            now: UtcTimestamp,
        ) -> Result<Run, OpenWorkError> {
            self.0
                .complete_leased_run(lease, revision, next, reason, now)
        }
        fn cancel_requested(
            &self,
            lease: &RunLease,
            now: UtcTimestamp,
        ) -> Result<bool, OpenWorkError> {
            self.0.lease_cancel_requested(lease, now)
        }
        fn confirm_cancel(
            &self,
            lease: &RunLease,
            now: UtcTimestamp,
            evidence: CancellationEvidence,
        ) -> Result<Run, OpenWorkError> {
            RunQueueRepository::confirm_cancel(&self.0, lease, now, evidence)
        }
    }

    struct NeverSandbox(AtomicBool);
    impl SandboxBackend for NeverSandbox {
        fn health(&self) -> Result<(), OpenWorkError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn execute(
            &self,
            _: &openwork_execution::SandboxRequest,
        ) -> Result<SandboxResult, OpenWorkError> {
            panic!("sandbox must not execute after cancellation intent")
        }
        fn cancel(&self, _: &RunId) -> Result<(), OpenWorkError> {
            Ok(())
        }
        fn cleanup(&self, _: &RunId) -> Result<(), OpenWorkError> {
            Ok(())
        }
    }

    struct BlockingSandbox {
        started: (Mutex<bool>, Condvar),
        cancelled: (Mutex<bool>, Condvar),
    }
    impl BlockingSandbox {
        fn new() -> Self {
            Self {
                started: (Mutex::new(false), Condvar::new()),
                cancelled: (Mutex::new(false), Condvar::new()),
            }
        }
        fn wait_started(&self) -> bool {
            let (lock, signal) = &self.started;
            let mut started = lock.lock().expect("lock");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !*started && std::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let (guard, _) = signal.wait_timeout(started, remaining).expect("wait");
                started = guard;
            }
            *started
        }
    }
    impl SandboxBackend for BlockingSandbox {
        fn health(&self) -> Result<(), OpenWorkError> {
            Ok(())
        }
        fn execute(
            &self,
            request: &openwork_execution::SandboxRequest,
        ) -> Result<SandboxResult, OpenWorkError> {
            let (lock, signal) = &self.started;
            *lock.lock().expect("lock") = true;
            signal.notify_all();
            let (lock, signal) = &self.cancelled;
            let mut cancelled = lock.lock().expect("lock");
            while !*cancelled {
                cancelled = signal.wait(cancelled).expect("wait");
            }
            Ok(SandboxResult {
                schema_version: openwork_execution::EXECUTION_SCHEMA_VERSION,
                run_id: request.run_id.clone(),
                sandbox_id: "blocking-sandbox".to_owned(),
                termination: SandboxTermination::Cancelled,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                truncated: false,
                started_at: UtcTimestamp::now(),
                completed_at: UtcTimestamp::now(),
                output_paths: Vec::new(),
                cleanup: openwork_execution::SandboxCleanupStatus::Succeeded,
            })
        }
        fn cancel(&self, _: &RunId) -> Result<(), OpenWorkError> {
            let (lock, signal) = &self.cancelled;
            *lock.lock().expect("lock") = true;
            signal.notify_all();
            Ok(())
        }
        fn cleanup(&self, _: &RunId) -> Result<(), OpenWorkError> {
            Ok(())
        }
    }

    #[test]
    fn cancellation_before_execution_fails_closed_without_exposing_prompt() {
        let store = StoreAdapter(InMemoryExecutionStore::default());
        let mut run = queued_run("secret prompt only for stdin");
        let environment = environment();
        run.workspace = environment.input_directory.as_path().to_path_buf();
        store
            .0
            .create_run(run.clone(), AuditAppend::new(actor(), run.created_at))
            .expect("create run");
        let lease = store
            .0
            .claim_next_run(
                actor(),
                timestamp("2026-08-22T00:00:01Z"),
                Duration::seconds(30),
            )
            .expect("claim")
            .expect("lease");
        store
            .0
            .request_cancel(&run.id, actor(), timestamp("2026-08-22T00:00:02Z"))
            .expect("intent");
        let sandbox = NeverSandbox(AtomicBool::new(false));
        let adapter = CodexTaskAdapter::new("/usr/bin/codex");
        let prompt = OneTimePrompt::new("secret prompt only for stdin".to_owned());
        assert!(!format!("{prompt:?}").contains("secret prompt"));
        let result = SingleRunWorker::new(&store, &sandbox, &adapter)
            .execute(
                lease,
                task_spec(&run),
                prompt,
                environment,
                StartDisposition::Run,
            )
            .expect("fail closed result");
        assert!(matches!(
            result,
            WorkerOutcome::Completed(Run {
                status: RunStatus::Failed,
                ..
            })
        ));
        assert!(!sandbox.0.load(Ordering::SeqCst));
        assert_eq!(
            store.0.get_run(&run.id).expect("read").expect("run").status,
            RunStatus::Failed
        );
    }

    #[test]
    fn running_worker_heartbeats_then_confirms_validated_cancellation() {
        let store = StoreAdapter(InMemoryExecutionStore::default());
        let mut run = queued_run("prompt");
        let environment = environment();
        run.workspace = environment.input_directory.as_path().to_path_buf();
        store
            .0
            .create_run(run.clone(), AuditAppend::new(actor(), run.created_at))
            .expect("create");
        let lease = store
            .0
            .claim_next_run(actor(), UtcTimestamp::now(), Duration::seconds(30))
            .expect("claim")
            .expect("lease");
        let run_id = run.id.clone();
        let sandbox = BlockingSandbox::new();
        let adapter = CodexTaskAdapter::new("/usr/bin/codex");
        std::thread::scope(|scope| {
            let worker = SingleRunWorker::new(&store, &sandbox, &adapter)
                .with_supervisor(SupervisorConfig {
                    heartbeat_interval: std::time::Duration::from_millis(5),
                    lease_ttl: std::time::Duration::from_secs(30),
                })
                .expect("supervisor");
            let handle = scope.spawn(move || {
                worker.execute(
                    lease,
                    task_spec(&run),
                    OneTimePrompt::new("prompt".to_owned()),
                    environment,
                    StartDisposition::Run,
                )
            });
            if !sandbox.wait_started() {
                sandbox.cancel(&run_id).expect("release sandbox");
                let result = handle.join().expect("worker thread");
                panic!("sandbox did not start: {result:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Err(error) = store
                .0
                .request_cancel(&run_id, actor(), UtcTimestamp::now())
            {
                sandbox.cancel(&run_id).expect("unblock sandbox");
                panic!("intent: {error:?}");
            }
            let outcome = handle.join().expect("worker thread").expect("outcome");
            assert!(matches!(
                outcome,
                WorkerOutcome::Completed(Run {
                    status: RunStatus::Cancelled,
                    ..
                })
            ));
        });
    }

    fn actor() -> ActorId {
        ActorId::parse("worker-test").expect("actor")
    }
    fn timestamp(_: &str) -> UtcTimestamp {
        UtcTimestamp::now()
    }
    fn queued_run(prompt: &str) -> Run {
        let now = UtcTimestamp::now();
        Run {
            schema_version: openwork_execution::EXECUTION_SCHEMA_VERSION,
            id: RunId::generate(),
            runtime: "codex".to_owned(),
            workspace: PathBuf::from("/workspace"),
            status: RunStatus::Queued,
            revision: 0,
            actor_id: actor(),
            prompt_sha256: sha256_bytes(prompt.as_bytes()),
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            terminal_reason: None,
        }
    }
    fn task_spec(run: &Run) -> WorkerTaskSpec {
        WorkerTaskSpec {
            runtime: "codex".to_owned(),
            prompt_sha256: run.prompt_sha256.clone(),
            working_directory: SandboxWorkingDirectory::parse("/workspace").expect("dir"),
            timeout_seconds: 30,
            capabilities: vec!["filesystem.read".to_owned()],
        }
    }
    fn environment() -> WorkerEnvironment {
        let root = tempfile::tempdir().expect("tempdir").keep();
        let input = root.join("input");
        let output = root.join("output");
        std::fs::create_dir_all(&input).expect("input");
        std::fs::create_dir_all(&output).expect("output");
        let input_directory =
            ApprovedMountDirectory::under_root(&input, &root).expect("input mount");
        let output_directory =
            ApprovedMountDirectory::under_root(&output, &root).expect("output mount");
        WorkerEnvironment {
            image: DigestPinnedImageRef::parse("docker.io/library/busybox@sha256:9db7fbc7c94ee6a0d8d0c4f1a1e2c17a8475a486e4ff1be2b7df7c5c1e6c0000").expect("image"),
            user: SandboxUser::new(1000, 1000).expect("user"),
            input_directory,
            artifact_output_root: output_directory.as_path().to_path_buf(),
            output_directory,
            limits: SandboxLimits::new(1000, 64 * 1024 * 1024, 64, 30, 1024).expect("limits"),
            network: SandboxNetworkPolicy::Disabled,
            max_artifact_bytes: 1024,
        }
    }
}
