//! Frozen M1 contracts for safe task execution.

use openwork_core::{ErrorCode, OpenWorkError, redact_json};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub const EXECUTION_SCHEMA_VERSION_NUMBER: u32 = 1;
pub const EXECUTION_SCHEMA_VERSION: SchemaVersion = SchemaVersion;
pub const DEFAULT_RUNTIME_TIMEOUT_SECONDS: u64 = 300;
pub const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_ACTION_PARAMETER_BYTES: usize = 64 * 1024;
pub const MAX_ACTION_PARAMETER_DEPTH: usize = 32;
pub const MAX_APPROVAL_TTL_SECONDS: i128 = 24 * 60 * 60;
pub const MAX_SANDBOX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaVersion;

impl Serialize for SchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(EXECUTION_SCHEMA_VERSION_NUMBER)
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version != EXECUTION_SCHEMA_VERSION_NUMBER {
            return Err(serde::de::Error::custom(
                "unsupported safe-execution schema version",
            ));
        }
        Ok(Self)
    }
}

macro_rules! uuid_v7_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            #[allow(clippy::missing_errors_doc)]
            pub fn parse(value: &str) -> Result<Self, OpenWorkError> {
                let uuid = Uuid::parse_str(value).map_err(|_| invalid_contract("invalid UUID"))?;
                if uuid.get_version_num() != 7 {
                    return Err(invalid_contract("execution IDs must be UUIDv7"));
                }
                Ok(Self(uuid))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let uuid = Uuid::deserialize(deserializer)?;
                if uuid.get_version_num() != 7 {
                    return Err(serde::de::Error::custom("execution IDs must be UUIDv7"));
                }
                Ok(Self(uuid))
            }
        }
    };
}

uuid_v7_id!(RunId);
uuid_v7_id!(ArtifactId);
uuid_v7_id!(AuditEventId);
uuid_v7_id!(ActionId);
uuid_v7_id!(ApprovalId);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UtcTimestamp(OffsetDateTime);

impl UtcTimestamp {
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        if !value.ends_with('Z') {
            return Err(invalid_contract(
                "timestamp must be RFC 3339 UTC with a Z suffix",
            ));
        }
        let parsed = OffsetDateTime::parse(&value, &Rfc3339)
            .map_err(|_| invalid_contract("timestamp must be RFC 3339 UTC with a Z suffix"))?;
        Ok(Self(parsed))
    }

    #[must_use]
    pub const fn unix_timestamp_nanos(self) -> i128 {
        self.0.unix_timestamp_nanos()
    }

    fn canonical_string(self) -> String {
        self.0
            .format(&Rfc3339)
            .expect("UTC timestamps always format as RFC 3339")
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.canonical_string();
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid_contract(
                "SHA-256 digest must be 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RelativeArtifactPath(String);

impl RelativeArtifactPath {
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        let invalid_segment = value.split('/').any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains(':')
        });
        if value.starts_with('/')
            || value.contains('\\')
            || value.contains('\0')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
            || invalid_segment
            || Path::new(&value).is_absolute()
        {
            return Err(invalid_contract(
                "artifact path must be a portable relative path below the output root",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RelativeArtifactPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SandboxWorkingDirectory(String);

impl SandboxWorkingDirectory {
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        if value != "/workspace"
            && (!value.starts_with("/workspace/")
                || value.ends_with('/')
                || value.contains("//")
                || value
                    .split('/')
                    .any(|segment| segment == ".." || segment == "."))
        {
            return Err(invalid_contract(
                "runtime working directory must remain below /workspace",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ArtifactSizeBytes(u64);

impl ArtifactSizeBytes {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(value: u64) -> Result<Self, OpenWorkError> {
        if value > DEFAULT_MAX_ARTIFACT_BYTES {
            return Err(invalid_contract("artifact exceeds the 100 MiB M1 limit"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ArtifactSizeBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for SandboxWorkingDirectory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ActorId(String);

impl ActorId {
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 256 {
            return Err(invalid_contract(
                "actor ID must contain 1 to 256 characters",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Planning,
    AwaitingApproval,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

impl RunStatus {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Queued,
                Self::Planning | Self::Failed | Self::Cancelled | Self::TimedOut
            ) | (
                Self::Planning,
                Self::AwaitingApproval
                    | Self::Running
                    | Self::Failed
                    | Self::Cancelled
                    | Self::TimedOut
            ) | (
                Self::AwaitingApproval,
                Self::Running | Self::Failed | Self::Cancelled | Self::TimedOut
            ) | (
                Self::Running,
                Self::AwaitingApproval
                    | Self::Succeeded
                    | Self::Failed
                    | Self::Cancelled
                    | Self::TimedOut
            )
        )
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub schema_version: SchemaVersion,
    pub id: RunId,
    pub runtime: String,
    pub workspace: PathBuf,
    pub status: RunStatus,
    pub revision: u64,
    pub actor_id: ActorId,
    pub prompt_sha256: Sha256Digest,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub started_at: Option<UtcTimestamp>,
    pub completed_at: Option<UtcTimestamp>,
    pub terminal_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub schema_version: SchemaVersion,
    pub id: ArtifactId,
    pub run_id: RunId,
    pub path: RelativeArtifactPath,
    pub media_type: String,
    pub size_bytes: ArtifactSizeBytes,
    pub sha256: Sha256Digest,
    pub created_at: UtcTimestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    RunCreated,
    RuntimeSelected,
    SandboxCreated,
    ActionRequested,
    PolicyAllowed,
    PolicyDenied,
    ApprovalRequested,
    ApprovalApproved,
    ApprovalDenied,
    RuntimeStarted,
    RuntimeOutput,
    ArtifactCreated,
    RuntimeCompleted,
    SandboxDestroyed,
    RunCompleted,
    RunFailed,
    ApprovalBindingMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RedactedAuditMetadata(BTreeMap<String, Value>);

impl RedactedAuditMetadata {
    #[must_use]
    pub fn from_untrusted(metadata: &BTreeMap<String, Value>) -> Self {
        let redacted = redact_json(&Value::Object(metadata.clone().into_iter().collect()));
        let Value::Object(entries) = redacted else {
            unreachable!("an object remains an object after redaction");
        };
        Self(entries.into_iter().collect())
    }

    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RedactedAuditMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let metadata = BTreeMap::<String, Value>::deserialize(deserializer)?;
        Ok(Self::from_untrusted(&metadata))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvent {
    pub schema_version: SchemaVersion,
    pub id: AuditEventId,
    pub run_id: RunId,
    pub sequence: u64,
    pub event_type: AuditEventType,
    pub actor: ActorId,
    pub timestamp: UtcTimestamp,
    pub metadata: RedactedAuditMetadata,
    pub previous_hash: Option<Sha256Digest>,
    event_hash: Sha256Digest,
}

impl AuditEvent {
    /// Creates a hash-bound audit event at an exact per-run sequence position.
    ///
    /// # Errors
    ///
    /// Returns an error for sequence zero or an invalid genesis/previous-hash
    /// relationship.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AuditEventId,
        run_id: RunId,
        sequence: u64,
        event_type: AuditEventType,
        actor: ActorId,
        timestamp: UtcTimestamp,
        metadata: RedactedAuditMetadata,
        previous_hash: Option<Sha256Digest>,
    ) -> Result<Self, OpenWorkError> {
        validate_audit_position(sequence, previous_hash.as_ref())?;
        let event_hash = audit_event_hash(
            &id,
            &run_id,
            sequence,
            event_type,
            &actor,
            timestamp,
            &metadata,
            previous_hash.as_ref(),
        );
        Ok(Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            id,
            run_id,
            sequence,
            event_type,
            actor,
            timestamp,
            metadata,
            previous_hash,
            event_hash,
        })
    }

    #[must_use]
    pub const fn event_hash(&self) -> &Sha256Digest {
        &self.event_hash
    }

    /// Verifies both the chain position and the canonical event digest.
    ///
    /// # Errors
    ///
    /// Returns an error when sequence, previous hash, or event hash differs
    /// from the expected append-only chain position.
    pub fn verify_integrity(
        &self,
        expected_sequence: u64,
        expected_previous: Option<&Sha256Digest>,
    ) -> Result<(), OpenWorkError> {
        validate_audit_position(self.sequence, self.previous_hash.as_ref())?;
        if self.sequence != expected_sequence
            || self.previous_hash.as_ref() != expected_previous
            || self.event_hash
                != audit_event_hash(
                    &self.id,
                    &self.run_id,
                    self.sequence,
                    self.event_type,
                    &self.actor,
                    self.timestamp,
                    &self.metadata,
                    self.previous_hash.as_ref(),
                )
        {
            return Err(invalid_contract("audit hash-chain integrity check failed"));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditEventWire {
    schema_version: SchemaVersion,
    id: AuditEventId,
    run_id: RunId,
    sequence: u64,
    event_type: AuditEventType,
    actor: ActorId,
    timestamp: UtcTimestamp,
    metadata: RedactedAuditMetadata,
    previous_hash: Option<Sha256Digest>,
    event_hash: Sha256Digest,
}

impl<'de> Deserialize<'de> for AuditEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AuditEventWire::deserialize(deserializer)?;
        let event = Self::new(
            wire.id,
            wire.run_id,
            wire.sequence,
            wire.event_type,
            wire.actor,
            wire.timestamp,
            wire.metadata,
            wire.previous_hash,
        )
        .map_err(serde::de::Error::custom)?;
        if wire.schema_version != EXECUTION_SCHEMA_VERSION || wire.event_hash != event.event_hash {
            return Err(serde::de::Error::custom(
                "audit event hash does not match canonical content",
            ));
        }
        Ok(event)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DigestPinnedImageRef(String);

impl DigestPinnedImageRef {
    #[allow(clippy::missing_errors_doc)]
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        let value = value.into();
        if !is_digest_pinned_image(&value) {
            return Err(invalid_contract(
                "sandbox image must be a valid lowercase OCI name pinned by sha256 digest",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DigestPinnedImageRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxCommand {
    program: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
}

impl SandboxCommand {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(
        program: PathBuf,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self, OpenWorkError> {
        let program_text = program
            .to_str()
            .ok_or_else(|| invalid_contract("sandbox program must be UTF-8"))?;
        let arguments_valid = arguments.len() <= 256
            && arguments
                .iter()
                .all(|argument| argument.len() <= 8192 && !argument.contains('\0'));
        let environment_valid = environment.len() <= 64
            && environment.iter().all(|(key, value)| {
                valid_environment_name(key) && value.len() <= 8192 && !value.contains('\0')
            });
        if !program.is_absolute()
            || program_text.contains('\0')
            || program_text.starts_with('-')
            || !arguments_valid
            || !environment_valid
        {
            return Err(invalid_contract(
                "sandbox command must use an absolute program and bounded argv/environment allowlist",
            ));
        }
        Ok(Self {
            program,
            arguments,
            environment,
        })
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxCommandWire {
    program: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for SandboxCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SandboxCommandWire::deserialize(deserializer)?;
        Self::new(wire.program, wire.arguments, wire.environment).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxLimits {
    cpu_millis: u64,
    memory_bytes: u64,
    pid_limit: u32,
    timeout_seconds: u64,
    max_output_bytes: u64,
}

impl SandboxLimits {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(
        cpu_millis: u64,
        memory_bytes: u64,
        pid_limit: u32,
        timeout_seconds: u64,
        max_output_bytes: u64,
    ) -> Result<Self, OpenWorkError> {
        if !(1..=64_000).contains(&cpu_millis)
            || !(1_048_576..=68_719_476_736).contains(&memory_bytes)
            || !(1..=4096).contains(&pid_limit)
            || !(1..=3600).contains(&timeout_seconds)
            || !(1..=MAX_SANDBOX_OUTPUT_BYTES).contains(&max_output_bytes)
        {
            return Err(invalid_contract("sandbox limits are outside M1 bounds"));
        }
        Ok(Self {
            cpu_millis,
            memory_bytes,
            pid_limit,
            timeout_seconds,
            max_output_bytes,
        })
    }

    #[must_use]
    pub const fn cpu_millis(self) -> u64 {
        self.cpu_millis
    }

    #[must_use]
    pub const fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    #[must_use]
    pub const fn pid_limit(self) -> u32 {
        self.pid_limit
    }

    #[must_use]
    pub const fn timeout_seconds(self) -> u64 {
        self.timeout_seconds
    }

    #[must_use]
    pub const fn max_output_bytes(self) -> u64 {
        self.max_output_bytes
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxLimitsWire {
    cpu_millis: u64,
    memory_bytes: u64,
    pid_limit: u32,
    timeout_seconds: u64,
    max_output_bytes: u64,
}

impl<'de> Deserialize<'de> for SandboxLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SandboxLimitsWire::deserialize(deserializer)?;
        Self::new(
            wire.cpu_millis,
            wire.memory_bytes,
            wire.pid_limit,
            wire.timeout_seconds,
            wire.max_output_bytes,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxUser {
    uid: u32,
    gid: u32,
}

impl SandboxUser {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(uid: u32, gid: u32) -> Result<Self, OpenWorkError> {
        if uid == 0 || gid == 0 {
            return Err(invalid_contract("sandbox user and group must be non-root"));
        }
        Ok(Self { uid, gid })
    }

    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxUserWire {
    uid: u32,
    gid: u32,
}

impl<'de> Deserialize<'de> for SandboxUser {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SandboxUserWire::deserialize(deserializer)?;
        Self::new(wire.uid, wire.gid).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ApprovedMountDirectory(PathBuf);

impl ApprovedMountDirectory {
    /// Canonicalizes a directory and proves that it is below an approved root.
    ///
    /// # Errors
    ///
    /// Returns an error if either path cannot be canonicalized, the target is
    /// not a directory, or it escapes the approved root.
    pub fn under_root(path: &Path, approved_root: &Path) -> Result<Self, OpenWorkError> {
        let root = fs::canonicalize(approved_root)
            .map_err(|_| invalid_contract("approved mount root is unavailable"))?;
        let canonical = fs::canonicalize(path)
            .map_err(|_| invalid_contract("sandbox mount directory is unavailable"))?;
        if !canonical.starts_with(&root)
            || canonical == root
            || !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(invalid_contract(
                "sandbox mount must be a directory below its approved root",
            ));
        }
        Ok(Self(canonical))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetworkPolicy {
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRequest {
    pub schema_version: SchemaVersion,
    pub run_id: RunId,
    pub image: DigestPinnedImageRef,
    pub command: SandboxCommand,
    pub user: SandboxUser,
    pub network: SandboxNetworkPolicy,
    pub input_directory: ApprovedMountDirectory,
    pub output_directory: ApprovedMountDirectory,
    pub limits: SandboxLimits,
}

impl SandboxRequest {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(
        run_id: RunId,
        image: DigestPinnedImageRef,
        command: SandboxCommand,
        user: SandboxUser,
        input_directory: ApprovedMountDirectory,
        output_directory: ApprovedMountDirectory,
        limits: SandboxLimits,
    ) -> Result<Self, OpenWorkError> {
        if input_directory == output_directory {
            return Err(invalid_contract(
                "sandbox input and output directories must be distinct",
            ));
        }
        Ok(Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            run_id,
            image,
            command,
            user,
            network: SandboxNetworkPolicy::Disabled,
            input_directory,
            output_directory,
            limits,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTermination {
    Exited,
    Cancelled,
    TimedOut,
    OutOfMemory,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxCleanupStatus {
    Succeeded,
    Failed { error_code: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxResult {
    pub schema_version: SchemaVersion,
    pub run_id: RunId,
    pub sandbox_id: String,
    pub termination: SandboxTermination,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub started_at: UtcTimestamp,
    pub completed_at: UtcTimestamp,
    pub output_paths: Vec<RelativeArtifactPath>,
    pub cleanup: SandboxCleanupStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxResultWire {
    schema_version: SchemaVersion,
    run_id: RunId,
    sandbox_id: String,
    termination: SandboxTermination,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    truncated: bool,
    started_at: UtcTimestamp,
    completed_at: UtcTimestamp,
    output_paths: Vec<RelativeArtifactPath>,
    cleanup: SandboxCleanupStatus,
}

impl<'de> Deserialize<'de> for SandboxResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SandboxResultWire::deserialize(deserializer)?;
        let result = Self {
            schema_version: wire.schema_version,
            run_id: wire.run_id,
            sandbox_id: wire.sandbox_id,
            termination: wire.termination,
            exit_code: wire.exit_code,
            stdout: wire.stdout,
            stderr: wire.stderr,
            truncated: wire.truncated,
            started_at: wire.started_at,
            completed_at: wire.completed_at,
            output_paths: wire.output_paths,
            cleanup: wire.cleanup,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}

impl SandboxResult {
    /// Validates bounded output, timestamps, exit semantics, and cleanup code.
    ///
    /// # Errors
    ///
    /// Returns an error when result fields contradict the sandbox termination
    /// or exceed M1 bounds.
    pub fn validate(&self) -> Result<(), OpenWorkError> {
        let exited_consistently = match self.termination {
            SandboxTermination::Exited => self.exit_code.is_some(),
            SandboxTermination::Cancelled
            | SandboxTermination::TimedOut
            | SandboxTermination::OutOfMemory => self.exit_code.is_none(),
            SandboxTermination::Failed => true,
        };
        let cleanup_valid = match &self.cleanup {
            SandboxCleanupStatus::Succeeded => true,
            SandboxCleanupStatus::Failed { error_code } => valid_machine_code(error_code),
        };
        if self.sandbox_id.is_empty()
            || self.sandbox_id.len() > 128
            || self.completed_at < self.started_at
            || self.stdout.len().saturating_add(self.stderr.len())
                > usize::try_from(MAX_SANDBOX_OUTPUT_BYTES).unwrap_or(usize::MAX)
            || self.output_paths.len() > 1024
            || !exited_consistently
            || !cleanup_valid
        {
            return Err(invalid_contract("sandbox result invariants are invalid"));
        }
        Ok(())
    }
}

#[allow(clippy::missing_errors_doc)]
pub trait SandboxBackend: Send + Sync {
    fn health(&self) -> Result<(), OpenWorkError>;
    fn execute(&self, request: &SandboxRequest) -> Result<SandboxResult, OpenWorkError>;
    fn cancel(&self, run_id: &RunId) -> Result<(), OpenWorkError>;
    fn cleanup(&self, run_id: &RunId) -> Result<(), OpenWorkError>;
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum RiskLevel {
    #[serde(rename = "L0")]
    Read,
    #[serde(rename = "L1")]
    LocalWrite,
    #[serde(rename = "L2")]
    InternalMutation,
    #[serde(rename = "L3")]
    ExternalEffect,
    #[serde(rename = "L4")]
    DestructiveOrFinancial,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActionRequest {
    pub schema_version: SchemaVersion,
    pub id: ActionId,
    pub run_id: RunId,
    pub action: String,
    pub resource: String,
    pub parameters: Value,
    parameter_hash: Sha256Digest,
}

impl ActionRequest {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(
        id: ActionId,
        run_id: RunId,
        action: impl Into<String>,
        resource: impl Into<String>,
        parameters: Value,
    ) -> Result<Self, OpenWorkError> {
        let action = action.into();
        let resource = resource.into();
        if action.trim().is_empty()
            || resource.trim().is_empty()
            || action.len() > 256
            || resource.len() > 2048
        {
            return Err(invalid_contract(
                "action and resource must be non-empty and bounded",
            ));
        }
        let parameter_hash = action_parameter_hash(&run_id, &id, &action, &resource, &parameters)?;
        Ok(Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            id,
            run_id,
            action,
            resource,
            parameters,
            parameter_hash,
        })
    }

    #[must_use]
    pub fn parameters_match_hash(&self) -> bool {
        action_parameter_hash(
            &self.run_id,
            &self.id,
            &self.action,
            &self.resource,
            &self.parameters,
        )
        .is_ok_and(|expected| self.parameter_hash == expected)
    }

    #[must_use]
    pub const fn parameter_hash(&self) -> &Sha256Digest {
        &self.parameter_hash
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny,
    RequireApproval,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvaluation {
    pub schema_version: SchemaVersion,
    pub run_id: RunId,
    pub action_id: ActionId,
    pub parameter_hash: Sha256Digest,
    pub decision: PolicyDecision,
    pub effective_risk: RiskLevel,
    pub policy_version: String,
    pub rule_id: String,
    pub reason_code: String,
    pub evaluated_at: UtcTimestamp,
}

pub trait ActionPolicy: Send + Sync {
    fn evaluate(&self, request: &ActionRequest) -> PolicyEvaluation;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Consumed,
}

impl ApprovalStatus {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Approved | Self::Denied | Self::Expired)
                | (Self::Approved, Self::Consumed | Self::Expired)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecisionRecord {
    pub decision: ApprovalDecision,
    pub actor: ActorId,
    pub reason: Option<String>,
    pub decided_at: UtcTimestamp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub schema_version: SchemaVersion,
    pub id: ApprovalId,
    pub run_id: RunId,
    pub action_id: ActionId,
    pub parameter_hash: Sha256Digest,
    pub requested_by: ActorId,
    pub request_reason: String,
    pub created_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub status: ApprovalStatus,
    pub revision: u64,
    pub decision: Option<ApprovalDecisionRecord>,
    pub consumed_at: Option<UtcTimestamp>,
}

impl ApprovalRequest {
    /// Validates TTL, state fields, and single-use decision consistency.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty reason, invalid TTL, or a state whose
    /// decision/consumption fields are inconsistent.
    pub fn validate(&self) -> Result<(), OpenWorkError> {
        if self.request_reason.trim().is_empty() {
            return Err(invalid_contract("approval request reason cannot be empty"));
        }
        let ttl_nanos =
            self.expires_at.unix_timestamp_nanos() - self.created_at.unix_timestamp_nanos();
        if ttl_nanos <= 0 || ttl_nanos > MAX_APPROVAL_TTL_SECONDS * 1_000_000_000 {
            return Err(invalid_contract(
                "approval TTL must be positive and no more than 24 hours",
            ));
        }
        let valid_state = match self.status {
            ApprovalStatus::Pending => self.decision.is_none() && self.consumed_at.is_none(),
            ApprovalStatus::Approved => {
                self.decision
                    .as_ref()
                    .is_some_and(|record| record.decision == ApprovalDecision::Approved)
                    && self.consumed_at.is_none()
            }
            ApprovalStatus::Denied => {
                self.decision
                    .as_ref()
                    .is_some_and(|record| record.decision == ApprovalDecision::Denied)
                    && self.consumed_at.is_none()
            }
            ApprovalStatus::Expired => self.consumed_at.is_none(),
            ApprovalStatus::Consumed => self.decision.as_ref().is_some_and(|record| {
                record.decision == ApprovalDecision::Approved
                    && self.consumed_at.is_some_and(|consumed| {
                        consumed >= record.decided_at && consumed < self.expires_at
                    })
            }),
        };
        if !valid_state {
            return Err(invalid_contract(
                "approval status is inconsistent with decision or consumption fields",
            ));
        }
        if self.decision.as_ref().is_some_and(|record| {
            record.decided_at < self.created_at || record.decided_at >= self.expires_at
        }) {
            return Err(invalid_contract(
                "approval decision must occur before the trusted expiry time",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn binding_matches(&self, action: &ActionRequest) -> bool {
        self.run_id == action.run_id
            && self.action_id == action.id
            && &self.parameter_hash == action.parameter_hash()
    }

    #[must_use]
    pub fn is_expired_at(&self, trusted_now: UtcTimestamp) -> bool {
        trusted_now >= self.expires_at
    }

    /// Checks the compare-and-swap revision, TTL, state, and exact binding
    /// required before a storage transaction consumes this approval.
    ///
    /// # Errors
    ///
    /// Returns an error unless the approval is valid, approved, unexpired, at
    /// the expected revision, and bound to the exact action.
    pub fn can_consume_at(
        &self,
        action: &ActionRequest,
        expected_revision: u64,
        trusted_now: UtcTimestamp,
    ) -> Result<(), OpenWorkError> {
        self.validate()?;
        if self.status != ApprovalStatus::Approved
            || self.revision != expected_revision
            || self.is_expired_at(trusted_now)
            || !self.binding_matches(action)
        {
            return Err(OpenWorkError::new(
                ErrorCode::ApprovalInvalid,
                "approval is expired, consumed, stale, or bound to another action",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRequestWire {
    schema_version: SchemaVersion,
    id: ApprovalId,
    run_id: RunId,
    action_id: ActionId,
    parameter_hash: Sha256Digest,
    requested_by: ActorId,
    request_reason: String,
    created_at: UtcTimestamp,
    expires_at: UtcTimestamp,
    status: ApprovalStatus,
    revision: u64,
    decision: Option<ApprovalDecisionRecord>,
    consumed_at: Option<UtcTimestamp>,
}

impl<'de> Deserialize<'de> for ApprovalRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ApprovalRequestWire::deserialize(deserializer)?;
        let request = Self {
            schema_version: wire.schema_version,
            id: wire.id,
            run_id: wire.run_id,
            action_id: wire.action_id,
            parameter_hash: wire.parameter_hash,
            requested_by: wire.requested_by,
            request_reason: wire.request_reason,
            created_at: wire.created_at,
            expires_at: wire.expires_at,
            status: wire.status,
            revision: wire.revision,
            decision: wire.decision,
            consumed_at: wire.consumed_at,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTask {
    pub schema_version: SchemaVersion,
    pub run_id: RunId,
    pub runtime: String,
    pub prompt: String,
    pub prompt_hash: Sha256Digest,
    pub working_directory: SandboxWorkingDirectory,
    pub timeout_seconds: u64,
    pub capabilities: Vec<String>,
}

impl RuntimeTask {
    /// Validates provider-neutral task bounds before a sandbox invocation is prepared.
    ///
    /// # Errors
    ///
    /// Returns an error for empty runtime names, stale prompt hashes, invalid
    /// timeouts, or duplicate/unbounded capability names.
    pub fn validate(&self) -> Result<(), OpenWorkError> {
        let capabilities_valid = self.capabilities.len() <= 64
            && self
                .capabilities
                .iter()
                .all(|capability| !capability.trim().is_empty() && capability.len() <= 128);
        let mut sorted = self.capabilities.clone();
        sorted.sort_unstable();
        let unique = sorted.windows(2).all(|pair| pair[0] != pair[1]);
        if self.runtime.trim().is_empty()
            || self.runtime.len() > 128
            || !(1..=3600).contains(&self.timeout_seconds)
            || !capabilities_valid
            || !unique
            || !self.prompt_matches_hash()
        {
            return Err(invalid_contract("runtime task invariants are invalid"));
        }
        Ok(())
    }

    #[must_use]
    pub fn prompt_matches_hash(&self) -> bool {
        self.prompt_hash == sha256_bytes(self.prompt.as_bytes())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTaskWire {
    schema_version: SchemaVersion,
    run_id: RunId,
    runtime: String,
    prompt: String,
    prompt_hash: Sha256Digest,
    working_directory: SandboxWorkingDirectory,
    timeout_seconds: u64,
    capabilities: Vec<String>,
}

impl<'de> Deserialize<'de> for RuntimeTask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RuntimeTaskWire::deserialize(deserializer)?;
        let task = Self {
            schema_version: wire.schema_version,
            run_id: wire.run_id,
            runtime: wire.runtime,
            prompt: wire.prompt,
            prompt_hash: wire.prompt_hash,
            working_directory: wire.working_directory,
            timeout_seconds: wire.timeout_seconds,
            capabilities: wire.capabilities,
        };
        task.validate().map_err(serde::de::Error::custom)?;
        Ok(task)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvent {
    pub schema_version: SchemaVersion,
    pub run_id: RunId,
    pub sequence: u64,
    pub timestamp: UtcTimestamp,
    pub payload: RuntimeEventPayload,
    pub vendor_metadata: RedactedAuditMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeEventPayload {
    Started,
    Stdout {
        chunk: String,
        truncated: bool,
    },
    Stderr {
        chunk: String,
        truncated: bool,
    },
    Message {
        content: String,
    },
    ToolCall {
        name: String,
        parameters: Value,
    },
    /// A provider claim only. Artifact discovery and validation belong to the
    /// execution output scanner, never to this event.
    Artifact {
        relative_path: String,
    },
    Completed {
        exit_code: i32,
    },
    Failed {
        code: String,
        message: String,
    },
    Cancelled,
}

/// Computes the frozen v1 binding used by policy and approval records.
///
/// # Errors
///
/// Returns an error when parameters contain floating-point values, exceed the
/// maximum nesting depth, or produce an encoded binding larger than 64 KiB.
pub fn action_parameter_hash(
    run_id: &RunId,
    action_id: &ActionId,
    action: &str,
    resource: &str,
    parameters: &Value,
) -> Result<Sha256Digest, OpenWorkError> {
    validate_parameter_value(parameters, 0)?;
    let binding = Value::Array(vec![
        Value::String("openwork-action-approval-v1".to_owned()),
        Value::String(run_id.0.to_string()),
        Value::String(action_id.0.to_string()),
        Value::String(action.to_owned()),
        Value::String(resource.to_owned()),
        canonical_json(parameters),
    ]);
    let bytes = binding.to_string().into_bytes();
    if bytes.len() > MAX_ACTION_PARAMETER_BYTES {
        return Err(invalid_contract("action binding exceeds the 64 KiB limit"));
    }
    Ok(sha256_bytes(&bytes))
}

#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    Sha256Digest(encoded)
}

#[must_use]
pub fn is_digest_pinned_image(image: &str) -> bool {
    if image.starts_with('-') || image.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    let mut segments = name.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    let valid_first = if let Some((host, port)) = first.rsplit_once(':') {
        valid_oci_segment(host)
            && !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        valid_oci_segment(first)
    };
    valid_first
        && segments.all(valid_oci_segment)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_oci_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    let last = segment.as_bytes().last().copied().unwrap_or_default();
    let edge_valid = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    edge_valid(first)
        && edge_valid(last)
        && segment.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn valid_environment_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    let mut bytes = name.bytes();
    let first = bytes.next().unwrap_or_default();
    (first.is_ascii_uppercase() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_machine_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 128
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn validate_parameter_value(value: &Value, depth: usize) -> Result<(), OpenWorkError> {
    if depth > MAX_ACTION_PARAMETER_DEPTH {
        return Err(invalid_contract(
            "action parameters exceed the maximum depth",
        ));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_parameter_value(value, depth + 1)?;
            }
        }
        Value::Object(entries) => {
            for value in entries.values() {
                validate_parameter_value(value, depth + 1)?;
            }
        }
        Value::Number(number) if !number.is_i64() && !number.is_u64() => {
            return Err(invalid_contract(
                "floating-point action parameters are not canonical in v1",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Object(entries) => {
            let sorted = entries
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn audit_event_hash(
    id: &AuditEventId,
    run_id: &RunId,
    sequence: u64,
    event_type: AuditEventType,
    actor: &ActorId,
    timestamp: UtcTimestamp,
    metadata: &RedactedAuditMetadata,
    previous_hash: Option<&Sha256Digest>,
) -> Sha256Digest {
    let binding = Value::Array(vec![
        Value::String("openwork-audit-event-v1".to_owned()),
        Value::String(id.0.to_string()),
        Value::String(run_id.0.to_string()),
        Value::Number(sequence.into()),
        Value::String(audit_event_type_name(event_type).to_owned()),
        Value::String(actor.as_str().to_owned()),
        Value::String(timestamp.canonical_string()),
        canonical_json(&Value::Object(metadata.0.clone().into_iter().collect())),
        previous_hash.map_or(Value::Null, |hash| Value::String(hash.0.clone())),
    ]);
    sha256_bytes(binding.to_string().as_bytes())
}

const fn audit_event_type_name(event_type: AuditEventType) -> &'static str {
    match event_type {
        AuditEventType::RunCreated => "run_created",
        AuditEventType::RuntimeSelected => "runtime_selected",
        AuditEventType::SandboxCreated => "sandbox_created",
        AuditEventType::ActionRequested => "action_requested",
        AuditEventType::PolicyAllowed => "policy_allowed",
        AuditEventType::PolicyDenied => "policy_denied",
        AuditEventType::ApprovalRequested => "approval_requested",
        AuditEventType::ApprovalApproved => "approval_approved",
        AuditEventType::ApprovalDenied => "approval_denied",
        AuditEventType::RuntimeStarted => "runtime_started",
        AuditEventType::RuntimeOutput => "runtime_output",
        AuditEventType::ArtifactCreated => "artifact_created",
        AuditEventType::RuntimeCompleted => "runtime_completed",
        AuditEventType::SandboxDestroyed => "sandbox_destroyed",
        AuditEventType::RunCompleted => "run_completed",
        AuditEventType::RunFailed => "run_failed",
        AuditEventType::ApprovalBindingMismatch => "approval_binding_mismatch",
    }
}

fn validate_audit_position(
    sequence: u64,
    previous_hash: Option<&Sha256Digest>,
) -> Result<(), OpenWorkError> {
    let valid =
        (sequence == 1 && previous_hash.is_none()) || (sequence > 1 && previous_hash.is_some());
    if !valid {
        return Err(invalid_contract(
            "audit sequence starts at 1 and non-genesis events require previous_hash",
        ));
    }
    Ok(())
}

fn invalid_contract(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::InvalidArguments, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_and_reverse_run_transitions_are_rejected() {
        let statuses = [
            RunStatus::Queued,
            RunStatus::Planning,
            RunStatus::AwaitingApproval,
            RunStatus::Running,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::TimedOut,
        ];
        let allowed = [
            (RunStatus::Queued, RunStatus::Planning),
            (RunStatus::Queued, RunStatus::Failed),
            (RunStatus::Queued, RunStatus::Cancelled),
            (RunStatus::Queued, RunStatus::TimedOut),
            (RunStatus::Planning, RunStatus::AwaitingApproval),
            (RunStatus::Planning, RunStatus::Running),
            (RunStatus::Planning, RunStatus::Failed),
            (RunStatus::Planning, RunStatus::Cancelled),
            (RunStatus::Planning, RunStatus::TimedOut),
            (RunStatus::AwaitingApproval, RunStatus::Running),
            (RunStatus::AwaitingApproval, RunStatus::Failed),
            (RunStatus::AwaitingApproval, RunStatus::Cancelled),
            (RunStatus::AwaitingApproval, RunStatus::TimedOut),
            (RunStatus::Running, RunStatus::AwaitingApproval),
            (RunStatus::Running, RunStatus::Succeeded),
            (RunStatus::Running, RunStatus::Failed),
            (RunStatus::Running, RunStatus::Cancelled),
            (RunStatus::Running, RunStatus::TimedOut),
        ];
        for current in statuses {
            for next in statuses {
                assert_eq!(
                    current.can_transition_to(next),
                    allowed.contains(&(current, next)),
                    "unexpected transition {current:?} -> {next:?}"
                );
            }
        }
        assert!(RunStatus::Succeeded.is_terminal());
    }

    #[test]
    fn schema_version_fails_closed_in_rust() {
        let result = serde_json::from_value::<RuntimeTask>(json!({
            "schema_version": 2,
            "run_id": run_id(),
            "runtime": "mock",
            "prompt": "safe task",
            "prompt_hash": sha256_bytes(b"safe task"),
            "working_directory": "/workspace",
            "timeout_seconds": 300,
            "capabilities": []
        }));
        assert!(result.is_err());
    }

    #[test]
    fn parameter_hash_is_independent_of_object_key_order() {
        let first = json!({"recipient": "finance@example.com", "subject": "report"});
        let second = json!({"subject": "report", "recipient": "finance@example.com"});
        let first_hash =
            action_parameter_hash(&run_id(), &action_id(), "email.send", "finance", &first)
                .expect("valid action binding");
        let second_hash =
            action_parameter_hash(&run_id(), &action_id(), "email.send", "finance", &second)
                .expect("valid action binding");
        assert_eq!(first_hash, second_hash);
        assert_eq!(
            first_hash.as_str(),
            "c55093b714d0a06dbceee0683f3f2a1cc644259efe28297a20cc27a6a701d9f4"
        );
    }

    #[test]
    fn changed_parameters_do_not_match_an_existing_approval_hash() {
        let original = json!({"recipient": "internal@example.com"});
        let changed = json!({"recipient": "external@example.net"});
        let mut request = ActionRequest::new(
            action_id(),
            run_id(),
            "email.send",
            "internal@example.com",
            original,
        )
        .expect("valid action");
        request.parameters = changed;
        assert!(!request.parameters_match_hash());
    }

    #[test]
    fn action_binding_changes_with_resource_or_action() {
        let parameters = json!({"amount": 10});
        let first = action_parameter_hash(
            &run_id(),
            &action_id(),
            "database.update",
            "orders",
            &parameters,
        );
        let changed = action_parameter_hash(
            &run_id(),
            &action_id(),
            "database.delete",
            "orders",
            &parameters,
        );
        assert_ne!(first, changed);
    }

    #[test]
    fn floating_point_action_parameters_fail_closed() {
        let result = ActionRequest::new(
            action_id(),
            run_id(),
            "database.update",
            "orders",
            json!({"amount": 1.5}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_image_requires_a_full_sha256_digest() {
        assert!(is_digest_pinned_image(&format!(
            "openwork/sandbox@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_digest_pinned_image("openwork/sandbox:latest"));
        assert!(!is_digest_pinned_image("openwork/sandbox@sha256:abcd"));
        assert!(!is_digest_pinned_image(&format!(
            "-openwork/sandbox@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_digest_pinned_image(&format!(
            "OpenWork/sandbox@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_digest_pinned_image(&format!(
            "openwork//sandbox@sha256:{}",
            "a".repeat(64)
        )));
    }

    #[test]
    fn unsafe_sandbox_values_cannot_deserialize() {
        assert!(serde_json::from_value::<SandboxUser>(json!({"uid": 0, "gid": 1000})).is_err());
        assert!(
            serde_json::from_value::<SandboxLimits>(json!({
                "cpu_millis": 0,
                "memory_bytes": 1_048_576,
                "pid_limit": 1,
                "timeout_seconds": 1,
                "max_output_bytes": 1
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<DigestPinnedImageRef>(json!("ubuntu:latest")).is_err());
        assert!(
            serde_json::from_value::<SandboxCommand>(json!({
                "program": "sh",
                "arguments": [],
                "environment": {"TOKEN": "must-not-inherit"}
            }))
            .is_err()
        );
    }

    #[test]
    fn valid_sandbox_request_has_no_caller_owned_temporary_path() {
        let root = tempfile::tempdir().expect("temp root");
        let input_root = root.path().join("inputs");
        let output_root = root.path().join("outputs");
        let input = input_root.join("run");
        let output = output_root.join("run");
        for directory in [&input_root, &output_root, &input, &output] {
            fs::create_dir(directory).expect("fixture directory");
        }
        let request = SandboxRequest::new(
            run_id(),
            DigestPinnedImageRef::parse(format!(
                "ghcr.io/openwork/sandbox@sha256:{}",
                "a".repeat(64)
            ))
            .expect("pinned image"),
            SandboxCommand::new(
                PathBuf::from("/usr/bin/mock-runtime"),
                vec!["run".to_owned()],
                BTreeMap::from([("LANG".to_owned(), "C.UTF-8".to_owned())]),
            )
            .expect("command"),
            SandboxUser::new(65_532, 65_532).expect("non-root"),
            ApprovedMountDirectory::under_root(&input, &input_root).expect("approved input"),
            ApprovedMountDirectory::under_root(&output, &output_root).expect("approved output"),
            SandboxLimits::new(1000, 268_435_456, 128, 300, 1_048_576).expect("limits"),
        )
        .expect("sandbox request");
        let serialized = serde_json::to_value(request).expect("serialize request");
        assert_eq!(serialized["schema_version"], 1);
        assert_eq!(serialized["network"], "disabled");
        assert!(serialized.get("temporary_directory").is_none());
        assert_eq!(serialized["user"]["uid"], 65_532);
    }

    #[test]
    fn contradictory_sandbox_result_is_rejected() {
        let result = serde_json::from_value::<SandboxResult>(json!({
            "schema_version": 1,
            "run_id": run_id(),
            "sandbox_id": "sandbox-1",
            "termination": "timed_out",
            "exit_code": 124,
            "stdout": "",
            "stderr": "",
            "truncated": false,
            "started_at": "2026-08-10T00:01:00Z",
            "completed_at": "2026-08-10T00:00:00Z",
            "output_paths": [],
            "cleanup": {"status": "succeeded"}
        }));
        assert!(result.is_err());
    }

    #[test]
    fn artifact_paths_are_portable_and_relative() {
        assert!(RelativeArtifactPath::parse("reports/summary.md").is_ok());
        assert!(RelativeArtifactPath::parse("../etc/passwd").is_err());
        assert!(RelativeArtifactPath::parse("C:\\secret.txt").is_err());
        assert!(RelativeArtifactPath::parse("/etc/passwd").is_err());
        assert!(RelativeArtifactPath::parse("reports//summary.md").is_err());
        assert!(RelativeArtifactPath::parse("reports/摘要.md").is_err());
        assert!(RelativeArtifactPath::parse("reports/bad\nname.md").is_err());
        assert!(ArtifactSizeBytes::new(DEFAULT_MAX_ARTIFACT_BYTES).is_ok());
        assert!(ArtifactSizeBytes::new(DEFAULT_MAX_ARTIFACT_BYTES + 1).is_err());
    }

    #[test]
    fn runtime_working_directory_stays_inside_the_sandbox_workspace() {
        assert!(SandboxWorkingDirectory::parse("/workspace").is_ok());
        assert!(SandboxWorkingDirectory::parse("/workspace/project").is_ok());
        assert!(SandboxWorkingDirectory::parse("/workspace/../secrets").is_err());
        assert!(SandboxWorkingDirectory::parse("/host/workspace").is_err());
        assert!(SandboxWorkingDirectory::parse("/workspace/").is_err());
    }

    #[test]
    fn approval_state_machine_is_single_use() {
        assert!(ApprovalStatus::Pending.can_transition_to(ApprovalStatus::Approved));
        assert!(ApprovalStatus::Approved.can_transition_to(ApprovalStatus::Consumed));
        assert!(!ApprovalStatus::Consumed.can_transition_to(ApprovalStatus::Approved));
        assert!(!ApprovalStatus::Denied.can_transition_to(ApprovalStatus::Consumed));
    }

    #[test]
    fn approval_expiry_uses_real_time_and_exact_binding() {
        let action = ActionRequest::new(
            action_id(),
            run_id(),
            "email.send",
            "finance",
            json!({"recipient": "finance@example.com"}),
        )
        .expect("valid action");
        let approval: ApprovalRequest = serde_json::from_value(json!({
            "schema_version": 1,
            "id": "01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a03",
            "run_id": run_id(),
            "action_id": action_id(),
            "parameter_hash": action.parameter_hash(),
            "requested_by": "user:requester",
            "request_reason": "external side effect",
            "created_at": "2026-08-10T00:00:00Z",
            "expires_at": "2026-08-10T00:05:00Z",
            "status": "approved",
            "revision": 1,
            "decision": {
                "decision": "approved",
                "actor": "user:approver",
                "reason": null,
                "decided_at": "2026-08-10T00:01:00Z"
            },
            "consumed_at": null
        }))
        .expect("valid approval");
        assert!(
            approval
                .can_consume_at(
                    &action,
                    1,
                    UtcTimestamp::parse("2026-08-10T00:04:59.999Z").expect("time")
                )
                .is_ok()
        );
        assert!(
            approval
                .can_consume_at(
                    &action,
                    1,
                    UtcTimestamp::parse("2026-08-10T00:05:00Z").expect("time")
                )
                .is_err()
        );
        assert!(
            UtcTimestamp::parse("2026-08-10T00:00:00.1Z").expect("time")
                > UtcTimestamp::parse("2026-08-10T00:00:00Z").expect("time")
        );
    }

    #[test]
    fn inconsistent_approval_state_is_rejected_on_deserialization() {
        let invalid = json!({
            "schema_version": 1,
            "id": "01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a03",
            "run_id": run_id(),
            "action_id": action_id(),
            "parameter_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "requested_by": "user:requester",
            "request_reason": "test",
            "created_at": "2026-08-10T00:00:00Z",
            "expires_at": "2026-08-10T00:05:00Z",
            "status": "consumed",
            "revision": 1,
            "decision": null,
            "consumed_at": null
        });
        assert!(serde_json::from_value::<ApprovalRequest>(invalid).is_err());
    }

    #[test]
    fn audit_metadata_is_redacted_on_construction_and_deserialization() {
        let metadata = BTreeMap::from([(
            "headers".to_owned(),
            json!({"Authorization": "Bearer visible", "safe": "kept"}),
        )]);
        let redacted = RedactedAuditMetadata::from_untrusted(&metadata);
        assert_eq!(redacted.as_map()["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(redacted.as_map()["headers"]["safe"], "kept");
    }

    #[test]
    fn audit_chain_starts_at_one_and_rejects_tampering() {
        let metadata = RedactedAuditMetadata::from_untrusted(&BTreeMap::from([(
            "reason_code".to_owned(),
            json!("created"),
        )]));
        let event = AuditEvent::new(
            AuditEventId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a04").expect("UUIDv7"),
            run_id(),
            1,
            AuditEventType::RunCreated,
            ActorId::parse("system:control-api").expect("actor"),
            UtcTimestamp::parse("2026-08-10T00:00:00Z").expect("time"),
            metadata,
            None,
        )
        .expect("genesis event");
        event.verify_integrity(1, None).expect("valid chain");
        assert_eq!(
            event.event_hash().as_str(),
            "fe41fcdce2311b633f76f8584375c3e670c6262191b7e6290f4113603b05f3d7"
        );
        assert!(
            AuditEvent::new(
                AuditEventId::generate(),
                run_id(),
                0,
                AuditEventType::RunCreated,
                ActorId::parse("system:test").expect("actor"),
                UtcTimestamp::now(),
                RedactedAuditMetadata::from_untrusted(&BTreeMap::new()),
                None,
            )
            .is_err()
        );

        let mut tampered = serde_json::to_value(event).expect("serialize event");
        tampered["metadata"]["reason_code"] = json!("changed");
        assert!(serde_json::from_value::<AuditEvent>(tampered).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let result = serde_json::from_value::<RuntimeTask>(json!({
            "schema_version": 1,
            "run_id": "01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a01",
            "runtime": "mock",
            "prompt": "safe task",
            "prompt_hash": sha256_bytes(b"safe task"),
            "working_directory": "/workspace",
            "timeout_seconds": 300,
            "capabilities": [],
            "auth_token": "must-not-enter-contracts"
        }));
        assert!(result.is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn checked_in_schema_matches_the_contract_version_and_definitions() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../contracts/schemas/safe-execution.v1.schema.json"
        ))
        .expect("safe-execution schema must be valid JSON");
        let validator = jsonschema::validator_for(&schema).expect("schema must compile");
        assert_eq!(schema["$defs"]["schemaVersion"]["const"], 1);
        for name in [
            "run",
            "artifact",
            "auditEvent",
            "sandboxRequest",
            "sandboxResult",
            "actionRequest",
            "policyEvaluation",
            "approvalRequest",
            "runtimeTask",
            "runtimeEvent",
        ] {
            assert!(schema["$defs"].get(name).is_some(), "missing {name}");
        }

        let valid_action = ActionRequest::new(
            action_id(),
            run_id(),
            "filesystem.write",
            "reports/summary.md",
            json!({"overwrite": false}),
        )
        .expect("valid action");
        assert!(validator.is_valid(&serde_json::to_value(valid_action).expect("serialize action")));

        let valid_task = json!({
            "schema_version": 1,
            "run_id": run_id(),
            "runtime": "mock",
            "prompt": "safe task",
            "prompt_hash": sha256_bytes(b"safe task"),
            "working_directory": "/workspace",
            "timeout_seconds": 300,
            "capabilities": ["filesystem.read"]
        });
        assert!(validator.is_valid(&valid_task));

        let invalid_float_action = json!({
            "schema_version": 1,
            "id": action_id(),
            "run_id": run_id(),
            "action": "database.update",
            "resource": "orders",
            "parameters": {"amount": 1.5},
            "parameter_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        assert!(!validator.is_valid(&invalid_float_action));

        let invalid_unpinned_sandbox = json!({
            "schema_version": 1,
            "run_id": run_id(),
            "image": "ubuntu:latest",
            "command": {"program": "/bin/true", "arguments": [], "environment": {}},
            "user": {"uid": 0, "gid": 0},
            "network": "disabled",
            "input_directory": "/approved/input",
            "output_directory": "/approved/output",
            "limits": {"cpu_millis": 0, "memory_bytes": 0, "pid_limit": 0, "timeout_seconds": 0, "max_output_bytes": 0}
        });
        assert!(!validator.is_valid(&invalid_unpinned_sandbox));

        let invalid_relative_program = json!({
            "schema_version": 1,
            "run_id": run_id(),
            "image": format!("ghcr.io/openwork/sandbox@sha256:{}", "a".repeat(64)),
            "command": {"program": "sh", "arguments": [], "environment": {}},
            "user": {"uid": 65532, "gid": 65532},
            "network": "disabled",
            "input_directory": "/approved/input",
            "output_directory": "/approved/output",
            "limits": {"cpu_millis": 1000, "memory_bytes": 268_435_456, "pid_limit": 128, "timeout_seconds": 300, "max_output_bytes": 1_048_576}
        });
        assert!(!validator.is_valid(&invalid_relative_program));

        let invalid_timeout_exit = json!({
            "schema_version": 1,
            "run_id": run_id(),
            "sandbox_id": "sandbox-1",
            "termination": "timed_out",
            "exit_code": 124,
            "stdout": "",
            "stderr": "",
            "truncated": false,
            "started_at": "2026-08-10T00:00:00Z",
            "completed_at": "2026-08-10T00:01:00Z",
            "output_paths": [],
            "cleanup": {"status": "succeeded"}
        });
        assert!(!validator.is_valid(&invalid_timeout_exit));

        let invalid_consumed_approval = json!({
            "schema_version": 1,
            "id": "01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a03",
            "run_id": run_id(),
            "action_id": action_id(),
            "parameter_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "requested_by": "user:requester",
            "request_reason": "test",
            "created_at": "2026-08-10T00:00:00Z",
            "expires_at": "2026-08-10T00:05:00Z",
            "status": "consumed",
            "revision": 1,
            "decision": null,
            "consumed_at": null
        });
        assert!(!validator.is_valid(&invalid_consumed_approval));

        let invalid_trailing_artifact_path = json!({
            "schema_version": 1,
            "id": "01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a05",
            "run_id": run_id(),
            "path": "reports/",
            "media_type": "text/plain",
            "size_bytes": 0,
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "created_at": "2026-08-10T00:00:00Z"
        });
        assert!(!validator.is_valid(&invalid_trailing_artifact_path));

        let invalid_whitespace_action = json!({
            "schema_version": 1,
            "id": action_id(),
            "run_id": run_id(),
            "action": " ",
            "resource": " ",
            "parameters": {},
            "parameter_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        assert!(!validator.is_valid(&invalid_whitespace_action));
    }

    fn run_id() -> RunId {
        RunId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a01").expect("valid UUIDv7")
    }

    fn action_id() -> ActionId {
        ActionId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a02").expect("valid UUIDv7")
    }
}
