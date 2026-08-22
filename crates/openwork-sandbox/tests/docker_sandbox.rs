use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{
    ApprovedMountDirectory, DigestPinnedImageRef, RunId, SandboxBackend, SandboxCleanupStatus,
    SandboxCommand, SandboxLimits, SandboxNetworkName, SandboxNetworkPolicy, SandboxRequest,
    SandboxTermination, SandboxUser,
};
use openwork_sandbox::{
    CapabilitySupport, CliOutput, ContainerEngineHealth, ContainerEngineKind, DockerCli,
    DockerSandbox, PodmanSandbox,
};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CONTAINER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InspectMode {
    Exit,
    Running,
    OutOfMemory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StdinDeliveryMode {
    Immediate,
    BlockUntilRelease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KillMode {
    Succeeds,
    SucceedsAfterStopDelay,
    Rejected,
}

struct FakeDockerCli {
    calls: Mutex<Vec<Vec<String>>>,
    inspect: Mutex<VecDeque<InspectMode>>,
    captured_environment: Mutex<String>,
    temporary_directory: Mutex<Option<PathBuf>>,
    output_directory: Mutex<Option<PathBuf>>,
    create_success: bool,
    start_transport_error: bool,
    kill_mode: KillMode,
    stdin_delivery: StdinDeliveryMode,
    stdin_release: Mutex<bool>,
    stdin_release_changed: Condvar,
    captured_stdin: Mutex<Vec<u8>>,
    remove_success: bool,
    logs: Vec<u8>,
}

impl FakeDockerCli {
    fn successful() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            inspect: Mutex::new(VecDeque::from([InspectMode::Running, InspectMode::Exit])),
            captured_environment: Mutex::new(String::new()),
            temporary_directory: Mutex::new(None),
            output_directory: Mutex::new(None),
            create_success: true,
            start_transport_error: false,
            kill_mode: KillMode::Succeeds,
            stdin_delivery: StdinDeliveryMode::Immediate,
            stdin_release: Mutex::new(false),
            stdin_release_changed: Condvar::new(),
            captured_stdin: Mutex::new(Vec::new()),
            remove_success: true,
            logs: b"safe output".to_vec(),
        }
    }

    fn commands(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn command_names(&self) -> Vec<String> {
        self.commands()
            .into_iter()
            .filter_map(|call| call.first().cloned())
            .collect()
    }

    fn release_stdin_delivery(&self) {
        *self.stdin_release.lock().expect("stdin release lock") = true;
        self.stdin_release_changed.notify_all();
    }

    fn wait_for_stdin_release(&self) -> Result<(), OpenWorkError> {
        let released = self.stdin_release.lock().expect("stdin release lock");
        let (released, timeout) = self
            .stdin_release_changed
            .wait_timeout_while(released, Duration::from_secs(2), |released| !*released)
            .expect("stdin release wait");
        if timeout.timed_out() && !*released {
            return Err(OpenWorkError::new(
                ErrorCode::ExecutionFailed,
                "simulated attached stdin remained blocked",
            ));
        }
        Ok(())
    }
}

impl DockerCli for FakeDockerCli {
    fn run(
        &self,
        arguments: &[OsString],
        max_output_bytes: u64,
        _timeout: Duration,
        stdin: &[u8],
    ) -> Result<CliOutput, OpenWorkError> {
        let args = arguments
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        self.calls.lock().expect("calls lock").push(args.clone());
        if !stdin.is_empty() {
            *self.captured_stdin.lock().expect("captured stdin lock") = stdin.to_vec();
        }
        match args.first().map(String::as_str) {
            Some("version") => Ok(output(true, b"27.0.0".to_vec(), max_output_bytes)),
            Some("info") => Ok(output(true, b"5.6.0".to_vec(), max_output_bytes)),
            Some("create") => {
                let cidfile = argument_after(&args, "--cidfile");
                fs::write(&cidfile, CONTAINER_ID).expect("fake cidfile");
                *self.temporary_directory.lock().expect("temporary lock") =
                    cidfile.parent().map(Path::to_path_buf);
                let environment = argument_after(&args, "--env-file");
                *self.captured_environment.lock().expect("environment lock") =
                    fs::read_to_string(environment).expect("fake environment");
                *self.output_directory.lock().expect("output lock") = args
                    .iter()
                    .find(|arg| arg.contains("dst=/workspace/output"))
                    .and_then(|mount| mount.split(',').find_map(|part| part.strip_prefix("src=")))
                    .map(PathBuf::from);
                Ok(output(self.create_success, Vec::new(), max_output_bytes))
            }
            Some("start") if self.start_transport_error => Err(OpenWorkError::new(
                ErrorCode::ExecutionFailed,
                "simulated start transport error",
            )),
            Some("start") => {
                if self.stdin_delivery == StdinDeliveryMode::BlockUntilRelease && !stdin.is_empty()
                {
                    self.wait_for_stdin_release()?;
                }
                if let Some(output_directory) =
                    self.output_directory.lock().expect("output lock").as_ref()
                {
                    fs::write(output_directory.join("summary.md"), "survives cleanup")
                        .expect("fake artifact");
                }
                Ok(output(true, Vec::new(), max_output_bytes))
            }
            Some("attach") => {
                if self.stdin_delivery == StdinDeliveryMode::BlockUntilRelease {
                    self.wait_for_stdin_release()?;
                }
                Ok(output(true, Vec::new(), max_output_bytes))
            }
            Some("inspect") => {
                let mode = self
                    .inspect
                    .lock()
                    .expect("inspect lock")
                    .pop_front()
                    .unwrap_or(InspectMode::Running);
                let state: &[u8] = match mode {
                    InspectMode::Exit => br#"{"Running":false,"OOMKilled":false,"ExitCode":0}"#,
                    InspectMode::Running => br#"{"Running":true,"OOMKilled":false,"ExitCode":0}"#,
                    InspectMode::OutOfMemory => {
                        br#"{"Running":false,"OOMKilled":true,"ExitCode":137}"#
                    }
                };
                Ok(output(true, state.to_vec(), max_output_bytes))
            }
            Some("logs") => Ok(output(true, self.logs.clone(), max_output_bytes)),
            Some("kill") => {
                self.release_stdin_delivery();
                if self.kill_mode == KillMode::SucceedsAfterStopDelay {
                    let mut inspect = self.inspect.lock().expect("inspect lock");
                    inspect.clear();
                    inspect.push_back(InspectMode::Exit);
                    drop(inspect);
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(output(
                    self.kill_mode != KillMode::Rejected,
                    Vec::new(),
                    max_output_bytes,
                ))
            }
            Some("rm") => Ok(output(self.remove_success, Vec::new(), max_output_bytes)),
            _ => panic!("unexpected Docker command: {args:?}"),
        }
    }
}

fn output(success: bool, bytes: Vec<u8>, limit: u64) -> CliOutput {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = bytes.len() > limit;
    CliOutput {
        success,
        stdout: bytes.into_iter().take(limit).collect(),
        stderr: Vec::new(),
        truncated,
    }
}

fn argument_after(args: &[String], flag: &str) -> PathBuf {
    let index = args.iter().position(|value| value == flag).expect("flag");
    PathBuf::from(args.get(index + 1).expect("flag value"))
}

fn normalized_create(arguments: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(arguments.len());
    let mut replace_next = false;
    for argument in arguments {
        if replace_next {
            normalized.push("<engine-owned-path>".to_owned());
            replace_next = false;
        } else if matches!(argument.as_str(), "--cidfile" | "--env-file") {
            normalized.push(argument.clone());
            replace_next = true;
        } else if argument.starts_with("type=bind,src=") {
            let target = argument.find(",dst=").expect("mount target");
            normalized.push(format!("type=bind,src=<approved>{}", &argument[target..]));
        } else {
            normalized.push(argument.clone());
        }
    }
    normalized
}

struct Fixture {
    _root: tempfile::TempDir,
    request: SandboxRequest,
    output: PathBuf,
    temporary: PathBuf,
}

fn fixture(timeout_seconds: u64, max_output_bytes: u64) -> Fixture {
    fixture_with_stdin(timeout_seconds, max_output_bytes, Vec::new())
}

fn fixture_with_stdin(timeout_seconds: u64, max_output_bytes: u64, stdin: Vec<u8>) -> Fixture {
    let root = tempfile::tempdir().expect("fixture root");
    let input_root = root.path().join("approved-inputs");
    let output_root = root.path().join("approved-outputs");
    let input = input_root.join("run");
    let output = output_root.join("run");
    let temporary = root.path().join("backend-temporary");
    for path in [&input_root, &output_root, &input, &output, &temporary] {
        fs::create_dir(path).expect("fixture directory");
    }
    let request = SandboxRequest::new(
        RunId::generate(),
        DigestPinnedImageRef::parse(format!(
            "ghcr.io/openwork/sandbox@sha256:{}",
            "1".repeat(64)
        ))
        .expect("image"),
        SandboxCommand::with_stdin(
            "/usr/bin/mock-runtime",
            vec!["run".to_owned()],
            BTreeMap::from([("EXPLICIT_TOKEN".to_owned(), "secret-value".to_owned())]),
            stdin,
        )
        .expect("command"),
        SandboxUser::new(65_532, 65_532).expect("user"),
        ApprovedMountDirectory::under_root(&input, &input_root).expect("input"),
        ApprovedMountDirectory::under_root(&output, &output_root).expect("output"),
        SandboxLimits::new(750, 134_217_728, 64, timeout_seconds, max_output_bytes)
            .expect("limits"),
    )
    .expect("request");
    Fixture {
        _root: root,
        request,
        output,
        temporary,
    }
}

#[test]
fn lifecycle_uses_hardened_create_then_id_only_cleanup() {
    let fixture = fixture(2, 1024);
    let cli = Arc::new(FakeDockerCli::successful());
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary.clone())
        .with_poll_interval(Duration::ZERO);
    let result = backend.execute(&fixture.request).expect("sandbox result");

    assert_eq!(result.termination, SandboxTermination::Exited);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "safe output");
    assert_eq!(result.cleanup, SandboxCleanupStatus::Succeeded);
    assert_eq!(result.output_paths[0].as_str(), "summary.md");
    assert_eq!(
        fs::read_to_string(fixture.output.join("summary.md")).unwrap(),
        "survives cleanup"
    );

    let calls = cli.commands();
    assert_eq!(calls[0][0], "create");
    assert_eq!(calls[1][0], "start");
    assert!(calls.iter().any(|call| call[0] == "inspect"));
    assert!(calls.iter().any(|call| call == &["kill", CONTAINER_ID]));
    assert!(
        calls
            .iter()
            .any(|call| call == &["rm", "--force", CONTAINER_ID])
    );
    assert!(!calls.iter().any(|call| call[0] == "run"));

    let create = &calls[0];
    for required in [
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        "134217728",
        "--pids-limit",
        "64",
        "--cpu-quota",
        "75000",
        "--env-file",
        "--cidfile",
    ] {
        assert!(
            create.iter().any(|arg| arg == required),
            "missing {required}"
        );
    }
    assert!(create.windows(2).any(|pair| pair == ["--network", "none"]));
    assert!(
        create
            .windows(2)
            .any(|pair| pair == ["--user", "65532:65532"])
    );
    assert!(
        create
            .iter()
            .any(|arg| { arg.contains("dst=/workspace/input") && arg.ends_with(",readonly") })
    );
    assert!(
        create
            .iter()
            .any(|arg| { arg.contains("dst=/workspace/output") && !arg.ends_with(",readonly") })
    );
    assert!(!create.iter().any(|arg| arg.contains("secret-value")));
    assert!(!create.iter().any(|arg| {
        arg.contains("docker.sock")
            || arg == "--privileged"
            || arg == "--pid=host"
            || arg == "--network=host"
    }));
    assert_eq!(
        cli.captured_environment.lock().unwrap().as_str(),
        "EXPLICIT_TOKEN=secret-value\n"
    );
    let temporary = cli.temporary_directory.lock().unwrap().clone().unwrap();
    assert!(
        !temporary.exists(),
        "backend temporary directory must be removed"
    );
}

#[test]
fn restricted_provider_network_is_forwarded_without_host_network_access() {
    let mut fixture = fixture(2, 1024);
    fixture.request.network = SandboxNetworkPolicy::Restricted(
        SandboxNetworkName::parse("openwork-provider-egress").expect("network"),
    );
    let cli = Arc::new(FakeDockerCli::successful());
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary.clone())
        .with_poll_interval(Duration::ZERO);
    backend.execute(&fixture.request).expect("sandbox result");
    let create = &cli.commands()[0];
    assert!(
        create
            .windows(2)
            .any(|pair| pair == ["--network", "openwork-provider-egress"])
    );
    assert!(!create.iter().any(|argument| argument == "host"));
}

#[test]
fn stdin_attachment_does_not_block_timeout_polling() {
    let prompt = b"provider prompt\n".to_vec();
    let fixture = fixture_with_stdin(1, 1024, prompt.clone());
    fs::write(fixture.output.join("prior.txt"), "keep").unwrap();
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::Running]));
    fake.stdin_delivery = StdinDeliveryMode::BlockUntilRelease;
    let cli = Arc::new(fake);
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary)
        .with_poll_interval(Duration::from_millis(2));
    let result = backend.execute(&fixture.request).expect("timeout result");

    assert_eq!(result.termination, SandboxTermination::TimedOut);
    let calls = cli.commands();
    assert!(calls.iter().any(|call| call == &["start", CONTAINER_ID]));
    assert!(
        calls
            .iter()
            .any(|call| { call == &["attach", "--sig-proxy=false", CONTAINER_ID] })
    );
    assert_eq!(*cli.captured_stdin.lock().unwrap(), prompt);
    assert!(
        cli.command_names()
            .windows(2)
            .any(|pair| pair == ["kill", "logs"])
    );
    assert!(cli.command_names().contains(&"rm".to_owned()));
    assert_eq!(
        fs::read_to_string(fixture.output.join("prior.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn stdin_attachment_does_not_block_cancel_polling() {
    let prompt = b"provider prompt\n".to_vec();
    let fixture = fixture_with_stdin(10, 1024, prompt.clone());
    let run_id = fixture.request.run_id.clone();
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::Running]));
    fake.stdin_delivery = StdinDeliveryMode::BlockUntilRelease;
    let cli = Arc::new(fake);
    let backend = Arc::new(
        DockerSandbox::new(Arc::clone(&cli), fixture.temporary)
            .with_poll_interval(Duration::from_millis(2)),
    );
    let executing = Arc::clone(&backend);
    let request = fixture.request;
    let handle = thread::spawn(move || executing.execute(&request));
    let deadline = Instant::now() + Duration::from_secs(1);
    let polling_started = loop {
        let names = cli.command_names();
        if names.contains(&"attach".to_owned()) && names.contains(&"inspect".to_owned()) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(2));
    };
    backend.cancel(&run_id).expect("cancel active sandbox");
    let result = handle
        .join()
        .expect("execute thread")
        .expect("cancel result");
    assert!(
        polling_started,
        "polling must begin while stdin is attached"
    );
    assert_eq!(result.termination, SandboxTermination::Cancelled);
    assert_eq!(*cli.captured_stdin.lock().unwrap(), prompt);
    assert!(cli.command_names().contains(&"rm".to_owned()));
    assert_eq!(
        backend.cancel(&run_id).unwrap_err().code,
        ErrorCode::RunCancelled
    );
}

#[test]
fn failed_cancel_transport_never_reports_cancelled() {
    let fixture = fixture(2, 1024);
    let run_id = fixture.request.run_id.clone();
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::Running, InspectMode::Exit]));
    fake.kill_mode = KillMode::Rejected;
    let cli = Arc::new(fake);
    let backend = Arc::new(
        DockerSandbox::new(Arc::clone(&cli), fixture.temporary)
            .with_poll_interval(Duration::from_millis(2)),
    );
    let executing = Arc::clone(&backend);
    let request = fixture.request;
    let handle = thread::spawn(move || executing.execute(&request));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !cli.command_names().contains(&"inspect".to_owned()) {
        assert!(Instant::now() < deadline, "sandbox did not start polling");
        thread::sleep(Duration::from_millis(2));
    }
    assert!(backend.cancel(&run_id).is_err());
    let result = handle.join().expect("execute thread").expect("result");
    assert_ne!(result.termination, SandboxTermination::Cancelled);
    assert!(cli.command_names().contains(&"rm".to_owned()));
}

#[test]
fn successful_cancel_cannot_race_stopped_container_inspection() {
    let fixture = fixture(10, 1024);
    let run_id = fixture.request.run_id.clone();
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::Running]));
    fake.kill_mode = KillMode::SucceedsAfterStopDelay;
    let cli = Arc::new(fake);
    let backend = Arc::new(
        DockerSandbox::new(Arc::clone(&cli), fixture.temporary)
            .with_poll_interval(Duration::from_millis(1)),
    );
    let executing = Arc::clone(&backend);
    let request = fixture.request;
    let handle = thread::spawn(move || executing.execute(&request));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !cli.command_names().contains(&"inspect".to_owned()) {
        assert!(Instant::now() < deadline, "sandbox did not start polling");
        thread::sleep(Duration::from_millis(1));
    }

    backend.cancel(&run_id).expect("kill accepted");
    let result = handle.join().expect("execute thread").expect("result");
    assert_eq!(result.termination, SandboxTermination::Cancelled);
}

#[test]
fn cancellation_with_cleanup_failure_is_not_a_terminal_cancel_claim() {
    let fixture = fixture_with_stdin(10, 1024, b"prompt".to_vec());
    let run_id = fixture.request.run_id.clone();
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::Running]));
    fake.stdin_delivery = StdinDeliveryMode::BlockUntilRelease;
    fake.remove_success = false;
    let cli = Arc::new(fake);
    let backend = Arc::new(
        DockerSandbox::new(Arc::clone(&cli), fixture.temporary)
            .with_poll_interval(Duration::from_millis(2)),
    );
    let executing = Arc::clone(&backend);
    let request = fixture.request;
    let handle = thread::spawn(move || executing.execute(&request));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !cli.command_names().contains(&"attach".to_owned()) {
        assert!(Instant::now() < deadline, "sandbox did not attach stdin");
        thread::sleep(Duration::from_millis(2));
    }
    backend.cancel(&run_id).expect("kill accepted");
    let result = handle.join().expect("execute thread").expect("result");
    assert_eq!(result.termination, SandboxTermination::Failed);
    assert!(matches!(
        result.cleanup,
        SandboxCleanupStatus::Failed { .. }
    ));
}

#[test]
fn start_transport_error_runs_guard_and_clears_registry() {
    let fixture = fixture(2, 1024);
    let run_id = fixture.request.run_id.clone();
    let mut fake = FakeDockerCli::successful();
    fake.start_transport_error = true;
    let cli = Arc::new(fake);
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary);
    assert!(backend.execute(&fixture.request).is_err());
    assert!(cli.command_names().contains(&"rm".to_owned()));
    assert_eq!(
        backend.cancel(&run_id).unwrap_err().code,
        ErrorCode::RunCancelled
    );
}

#[test]
fn failed_create_with_cidfile_is_still_recovered_by_id() {
    let fixture = fixture(2, 1024);
    let mut fake = FakeDockerCli::successful();
    fake.create_success = false;
    let cli = Arc::new(fake);
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary);
    assert!(backend.execute(&fixture.request).is_err());
    let calls = cli.commands();
    assert!(calls.iter().any(|call| call == &["kill", CONTAINER_ID]));
    assert!(
        calls
            .iter()
            .any(|call| call == &["rm", "--force", CONTAINER_ID])
    );
}

#[test]
fn output_is_bounded_and_oom_is_distinct() {
    let fixture = fixture(2, 8);
    let mut fake = FakeDockerCli::successful();
    fake.inspect = Mutex::new(VecDeque::from([InspectMode::OutOfMemory]));
    fake.logs = b"0123456789abcdef".to_vec();
    let backend = DockerSandbox::new(Arc::new(fake), fixture.temporary);
    let result = backend.execute(&fixture.request).expect("OOM result");
    assert_eq!(result.termination, SandboxTermination::OutOfMemory);
    assert_eq!(result.stdout.len(), 8);
    assert!(result.truncated);
}

#[test]
fn cleanup_failure_is_machine_readable_in_result() {
    let fixture = fixture(2, 1024);
    let mut fake = FakeDockerCli::successful();
    fake.remove_success = false;
    let backend = DockerSandbox::new(Arc::new(fake), fixture.temporary);
    let result = backend
        .execute(&fixture.request)
        .expect("cleanup failure result");
    assert_eq!(
        result.cleanup,
        SandboxCleanupStatus::Failed {
            error_code: "docker.remove_failed".to_owned()
        }
    );
}

#[test]
fn oversized_output_tree_fails_without_recursive_scanner_growth() {
    let fixture = fixture(2, 1024);
    for index in 0..4097 {
        fs::create_dir(fixture.output.join(format!("directory-{index}"))).unwrap();
    }
    let backend = DockerSandbox::new(Arc::new(FakeDockerCli::successful()), fixture.temporary);
    let result = backend
        .execute(&fixture.request)
        .expect("bounded scan result");
    assert_eq!(result.termination, SandboxTermination::Failed);
    assert!(result.output_paths.is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_output_is_rejected_as_untrusted_artifact() {
    use std::os::unix::fs::symlink;
    let fixture = fixture(2, 1024);
    symlink("/etc/passwd", fixture.output.join("escape.txt")).expect("output symlink");
    let backend = DockerSandbox::new(Arc::new(FakeDockerCli::successful()), fixture.temporary);
    let result = backend
        .execute(&fixture.request)
        .expect("failed sandbox result");
    assert_eq!(result.termination, SandboxTermination::Failed);
    assert!(result.output_paths.is_empty());
}

#[cfg(unix)]
#[test]
fn mount_replaced_by_symlink_after_approval_fails_closed() {
    use std::os::unix::fs::symlink;
    let fixture = fixture(2, 1024);
    let input = fixture.request.input_directory.as_path();
    fs::remove_dir(input).expect("remove approved input");
    symlink("/etc", input).expect("replace input with symlink");
    let cli = Arc::new(FakeDockerCli::successful());
    let backend = DockerSandbox::new(Arc::clone(&cli), fixture.temporary);
    assert_eq!(
        backend.execute(&fixture.request).unwrap_err().code,
        ErrorCode::InvalidArguments
    );
    assert!(
        cli.commands().is_empty(),
        "Docker must not run for an unsafe mount"
    );
}

#[test]
fn docker_and_podman_adapters_share_hardened_lifecycle() {
    let fixture = fixture(2, 1024);
    let docker_cli = Arc::new(FakeDockerCli::successful());
    let docker = DockerSandbox::new(Arc::clone(&docker_cli), fixture.temporary.clone())
        .with_poll_interval(Duration::ZERO);
    assert_eq!(docker.engine_status().kind, ContainerEngineKind::Docker);
    assert_eq!(
        docker.engine_status().health,
        ContainerEngineHealth::NotChecked
    );
    docker.health().expect("healthy fake Docker daemon");
    docker.execute(&fixture.request).expect("Docker lifecycle");

    let podman_cli = Arc::new(FakeDockerCli::successful());
    let podman = PodmanSandbox::new(Arc::clone(&podman_cli), fixture.temporary.clone())
        .with_poll_interval(Duration::ZERO);
    let podman_status = podman.engine_status();
    assert_eq!(podman_status.kind, ContainerEngineKind::Podman);
    assert_eq!(podman_status.health, ContainerEngineHealth::NotChecked);
    assert_eq!(
        podman_status.capabilities.resource_limits,
        CapabilitySupport::HostDependent
    );
    podman.health().expect("healthy fake Podman engine");
    podman.execute(&fixture.request).expect("Podman lifecycle");

    assert_eq!(
        docker.engine_status().health,
        ContainerEngineHealth::Available
    );
    assert_eq!(
        podman.engine_status().health,
        ContainerEngineHealth::Available
    );
    let docker_calls = docker_cli.commands();
    let podman_calls = podman_cli.commands();
    assert_eq!(
        docker_calls.first().unwrap(),
        &["version", "--format", "{{.Server.Version}}"]
    );
    assert_eq!(
        podman_calls.first().unwrap(),
        &["info", "--format", "{{.Version.Version}}"]
    );
    let docker_create = docker_calls
        .iter()
        .find(|call| call.first().is_some_and(|name| name == "create"))
        .expect("Docker create");
    let podman_create = podman_calls
        .iter()
        .find(|call| call.first().is_some_and(|name| name == "create"))
        .expect("Podman create");
    assert_eq!(
        normalized_create(docker_create),
        normalized_create(podman_create)
    );
    for command in ["start", "inspect", "logs", "kill", "rm"] {
        assert!(docker_cli.command_names().contains(&command.to_owned()));
        assert!(podman_cli.command_names().contains(&command.to_owned()));
    }
}

#[cfg(unix)]
#[test]
fn system_cli_clears_environment_and_bounds_output() {
    use openwork_sandbox::{SystemDockerCli, SystemPodmanCli};
    let cli = SystemDockerCli::new(PathBuf::from("/usr/bin/env"))
        .expect("absolute executable")
        .with_cli_environment(OsString::from("EXPLICIT"), OsString::from("visible"));
    let environment = cli
        .run(&[], 1024, Duration::from_secs(2), &[])
        .expect("env output");
    assert_eq!(
        String::from_utf8(environment.stdout).unwrap(),
        "EXPLICIT=visible\n"
    );

    let printf = SystemDockerCli::new(PathBuf::from("/usr/bin/printf")).unwrap();
    let bounded = printf
        .run(
            &[OsString::from("0123456789abcdef")],
            7,
            Duration::from_secs(2),
            &[],
        )
        .expect("printf output");
    assert_eq!(bounded.stdout.len() + bounded.stderr.len(), 7);
    assert!(bounded.truncated);

    let sleep = SystemDockerCli::new(PathBuf::from("/bin/sleep")).unwrap();
    let timeout = sleep
        .run(&[OsString::from("2")], 16, Duration::from_millis(10), &[])
        .unwrap_err();
    assert_eq!(timeout.code, ErrorCode::RunTimedOut);

    let podman = SystemPodmanCli::new(PathBuf::from("relative/podman")).unwrap_err();
    assert_eq!(podman.code, ErrorCode::InvalidArguments);
    assert_eq!(podman.message, "Podman executable must be an absolute path");
}
