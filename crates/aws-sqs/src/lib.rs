//! SQS operations for the standalone AWS client.
//!
//! Native IAM Identity Center and SQS APIs support workforce sign-in. AWS CLI
//! Console login uses an isolated app-owned profile and credential cache.

mod native;
pub use native::{Account, DeviceAuthorization, IdentityCenter, Role, RoleCredentials, SsoSession};
mod login_settings;
pub use login_settings::{LoginMethod, LoginSettings};

use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Extract the HTTPS authorization page from one AWS CLI SSO login line.
pub fn sso_device_url(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|part| part.starts_with("https://"))
        .map(|part| part.trim_end_matches(['.', ',', ')']).to_string())
}

/// Extract the short device code from one AWS CLI SSO login line.
pub fn sso_device_code(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|part| part.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .find(|part| {
            let mut halves = part.split('-');
            let first = halves.next().unwrap_or_default();
            let second = halves.next().unwrap_or_default();
            halves.next().is_none()
                && (4..=8).contains(&first.len())
                && (4..=8).contains(&second.len())
                && first
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && second
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
        .map(str::to_string)
}

/// Build a shell-safe AWS CLI setup command for the selected profile.
pub fn sso_setup_command(profile: &str) -> String {
    let profile = profile.trim();
    if profile.is_empty() {
        "aws configure sso".to_string()
    } else {
        format!(
            "aws configure sso --profile '{}'",
            profile.replace('\'', "'\\''")
        )
    }
}

/// An AWS CLI invocation failed or returned an unexpected response.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An AWS API request failed without exposing a credential or token.
    #[error("AWS API failed: {0}")]
    Api(String),
    /// An AWS region or Identity Center start URL was invalid.
    #[error("invalid {0}")]
    InvalidEndpoint(&'static str),
    /// The device authorization expired before the browser sign-in completed.
    #[error("device authorization expired; sign in again")]
    DeviceExpired,
    /// The local browser callback could not be started or read.
    #[error("browser sign-in callback failed: {0}")]
    CallbackIo(std::io::Error),
    /// Credentials have expired and a new sign-in is required.
    #[error("AWS credentials expired; sign in again")]
    CredentialsExpired,
    /// The configured CLI could not be started.
    #[error("could not start AWS CLI: {0}")]
    Start(#[from] std::io::Error),
    /// The selected profile has no IAM Identity Center start URL or SSO region.
    #[error("IAM Identity Center is not configured for this profile")]
    SsoNotConfigured,
    /// The CLI rejected the SQS operation.
    #[error("AWS CLI failed: {0}")]
    Command(String),
    /// The CLI returned output outside the documented JSON shape.
    #[error("invalid AWS CLI response: {0}")]
    Response(#[from] serde_json::Error),
    /// A required input was empty.
    #[error("{0} is required")]
    Missing(&'static str),
    /// The queue name does not meet SQS naming rules.
    #[error(
        "queue name must be 1–80 ASCII letters, digits, hyphens or underscores; FIFO names end in .fifo"
    )]
    InvalidQueueName,
    /// A dead-letter queue ARN cannot be resolved to an SQS queue name and owner.
    #[error("invalid SQS queue ARN")]
    InvalidQueueArn,
    /// SQS accepts a receive wait from zero through twenty seconds.
    #[error("receive wait must be between 0 and 20 seconds")]
    InvalidReceiveWait,
    /// SQS accepts a receive batch size from one through ten.
    #[error("receive count must be between 1 and 10")]
    InvalidReceiveCount,
    /// SQS accepts a queue-wide receive wait from zero through twenty seconds.
    #[error("queue receive wait must be between 0 and 20 seconds")]
    InvalidQueueReceiveWait,
    /// SQS accepts a message visibility timeout from zero through twelve hours.
    #[error("visibility timeout must be between 0 and 43200 seconds")]
    InvalidVisibilityTimeout,
    /// SQS accepts a per-message delay from zero through fifteen minutes.
    #[error("send delay must be between 0 and 900 seconds")]
    InvalidSendDelay,
    /// SQS accepts a queue-wide delay from zero through fifteen minutes.
    #[error("queue delay must be between 0 and 900 seconds")]
    InvalidQueueDelay,
    /// SQS accepts a queue-wide visibility timeout from zero through twelve hours.
    #[error("queue visibility timeout must be between 0 and 43200 seconds")]
    InvalidQueueVisibilityTimeout,
    /// SQS accepts a queue message retention period from one minute through fourteen days.
    #[error("queue message retention period must be between 60 and 1209600 seconds")]
    InvalidQueueMessageRetentionPeriod,
    /// FIFO queues allow a delay only at the queue level.
    #[error("FIFO queues do not support per-message delay")]
    FifoMessageDelay,
    /// The string attribute editor requires a JSON object with string values.
    #[error("message attributes must be a JSON object of string names and values")]
    InvalidMessageAttributes,
    /// SQS permits at most ten custom attributes on a message.
    #[error("a message can have at most 10 attributes")]
    TooManyMessageAttributes,
    /// An attribute name violates SQS naming rules.
    #[error("invalid message attribute name: {0}")]
    InvalidMessageAttributeName(String),
    /// An attribute value must contain at least one character.
    #[error("message attribute value is required: {0}")]
    EmptyMessageAttributeValue(String),
}

/// The configured AWS identity and optional explicit region.
#[derive(Clone)]
pub struct AwsCli {
    executable: PathBuf,
    profile: Option<String>,
    region: Option<String>,
    native: Option<native::NativeSqs>,
    require_sign_in: bool,
    console_home: Option<PathBuf>,
}

/// Queue settings and approximate counts from SQS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueAttributes {
    /// ARN used by SQS to identify this queue.
    pub queue_arn: Option<String>,
    /// ARN of the configured dead-letter queue, when redrive is configured.
    pub dead_letter_target_arn: Option<String>,
    /// Messages available to receive.
    pub available: u64,
    /// Messages in flight and hidden by a visibility timeout.
    pub in_flight: u64,
    /// Messages waiting for a delay period to end.
    pub delayed: u64,
    /// Default delivery delay in seconds, or `None` if the CLI omitted it.
    pub delay_seconds: Option<u32>,
    /// Default visibility timeout in seconds, or `None` if the CLI omitted it.
    pub visibility_timeout: Option<u32>,
    /// Default receive wait in seconds, or `None` if the CLI omitted it.
    pub receive_wait_seconds: Option<u8>,
    /// Message retention period in seconds, or `None` if the CLI omitted it.
    pub message_retention_period: Option<u32>,
}

/// A received message and its transient deletion handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// SQS message identifier.
    pub id: String,
    /// UTF-8 message body.
    pub body: String,
    /// Receipt handle for this receive attempt; required for deletion.
    pub receipt_handle: String,
    /// Number of times SQS has delivered the message, when returned.
    pub receive_count: Option<u64>,
    /// Original send time in Unix milliseconds, when returned.
    pub sent_at_millis: Option<u64>,
    /// FIFO message group, when returned.
    pub group_id: Option<String>,
    /// Custom attributes requested with this receive, keyed by name.
    pub message_attributes: BTreeMap<String, MessageAttribute>,
}

/// One SQS custom message attribute returned by the AWS CLI.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct MessageAttribute {
    /// SQS data type, including any custom type suffix.
    pub data_type: String,
    /// Value of a String or Number attribute, when present.
    #[serde(default)]
    pub string_value: Option<String>,
    /// Base64 representation of a Binary attribute, when present.
    #[serde(default)]
    pub binary_value: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct QueuesOutput {
    #[serde(default)]
    queue_urls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreateQueueOutput {
    queue_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AttributesOutput {
    attributes: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedrivePolicy {
    dead_letter_target_arn: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct QueueUrlOutput {
    queue_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MoveTaskOutput {
    task_handle: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MessagesOutput {
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawMessage {
    message_id: String,
    body: String,
    receipt_handle: String,
    #[serde(default)]
    attributes: std::collections::HashMap<String, String>,
    #[serde(default)]
    message_attributes: BTreeMap<String, MessageAttribute>,
}

/// Parse the optional string-attribute editor value. Empty text sends no attributes.
/// Only JSON objects of string values are accepted; names and values follow SQS rules.
pub fn parse_string_message_attributes(input: &str) -> Result<BTreeMap<String, String>, Error> {
    if input.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let attributes: BTreeMap<String, String> =
        serde_json::from_str(input).map_err(|_| Error::InvalidMessageAttributes)?;
    validate_message_attributes(&attributes)?;
    Ok(attributes)
}

fn validate_message_attributes(attributes: &BTreeMap<String, String>) -> Result<(), Error> {
    if attributes.len() > 10 {
        return Err(Error::TooManyMessageAttributes);
    }
    for (name, value) in attributes {
        let lower = name.to_ascii_lowercase();
        if name.is_empty()
            || name.len() > 256
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || name.starts_with('.')
            || name.ends_with('.')
            || name.contains("..")
            || lower.starts_with("aws.")
            || lower.starts_with("amazon.")
        {
            return Err(Error::InvalidMessageAttributeName(name.clone()));
        }
        if value.is_empty() {
            return Err(Error::EmptyMessageAttributeValue(name.clone()));
        }
    }
    Ok(())
}

impl AwsCli {
    /// Use the AWS CLI on PATH, with its ordinary default profile and region.
    pub fn new() -> Self {
        Self {
            executable: PathBuf::from("aws"),
            profile: None,
            region: None,
            native: None,
            require_sign_in: false,
            console_home: None,
        }
    }

    /// Use temporary IAM Identity Center role credentials directly, without AWS CLI.
    pub fn native(region: impl Into<String>, credentials: RoleCredentials) -> Result<Self, Error> {
        Ok(Self {
            native: Some(native::NativeSqs::new(region.into(), credentials)?),
            ..Self::new()
        })
    }

    /// Reject queue requests until the app has selected an Identity Center role.
    pub fn unauthenticated() -> Self {
        Self {
            require_sign_in: true,
            ..Self::new()
        }
    }

    /// Select an AWS shared-config profile; an empty value means default.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = nonempty(profile.into());
        self
    }

    /// Select a region; an empty value lets the AWS CLI resolve it.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = nonempty(region.into());
        self
    }

    /// Replace the executable, mainly for scripted tests or a custom install.
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Use an app-owned AWS CLI Console login profile and credential cache.
    pub fn console(region: impl Into<String>, home: impl Into<PathBuf>) -> Result<Self, Error> {
        let region = region.into();
        validate_region(&region)?;
        Ok(Self {
            profile: Some("ginka-console".into()),
            region: Some(region),
            console_home: Some(home.into()),
            ..Self::new()
        })
    }

    /// Open the AWS Console browser sign-in through AWS CLI 2.32 or later.
    pub fn console_login(&self) -> Result<(), Error> {
        let Some(home) = &self.console_home else {
            return Err(Error::Missing("Console login configuration"));
        };
        self.prepare_console_home(home)?;
        let mut command = Command::new(&self.executable);
        self.console_environment(&mut command, home);
        command.args(["login", "--profile", "ginka-console", "--region"]);
        command.arg(self.region.as_deref().unwrap_or_default());
        let output = command.stdin(Stdio::null()).output()?;
        if !output.status.success() {
            return Err(command_error(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }

    fn prepare_console_home(&self, home: &std::path::Path) -> Result<(), Error> {
        std::fs::create_dir_all(home.join("login-cache"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(
                home.join("login-cache"),
                std::fs::Permissions::from_mode(0o700),
            )?;
        }
        let config = home.join("config");
        // An explicit sign-in must not reuse the previous account's
        // login_session: AWS CLI asks for confirmation on stdin when it does.
        let region = self.region.as_deref().unwrap_or_default();
        std::fs::write(
            &config,
            format!("[profile ginka-console]\nregion = {region}\n"),
        )?;
        // The CLI gives a credentials file precedence over a login session.
        // Keep this isolated file empty even if an earlier run left keys in it.
        let credentials = home.join("credentials");
        std::fs::write(&credentials, "")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))?;
            std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn console_environment(&self, command: &mut Command, home: &std::path::Path) {
        command.env("AWS_CONFIG_FILE", home.join("config"));
        command.env("AWS_SHARED_CREDENTIALS_FILE", home.join("credentials"));
        command.env("AWS_LOGIN_CACHE_DIRECTORY", home.join("login-cache"));
        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_SECURITY_TOKEN",
            "AWS_PROFILE",
            "AWS_DEFAULT_PROFILE",
            "AWS_ROLE_ARN",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        ] {
            command.env_remove(key);
        }
    }

    /// Complete IAM Identity Center device authorization for the selected profile.
    pub fn sso_login(&self) -> Result<(), Error> {
        self.sso_login_with_progress(|_| {})
    }

    /// Stream the device authorization instructions while the CLI waits for login.
    /// The callback runs on the calling thread and receives one output line at a time.
    pub fn sso_login_with_progress(&self, mut progress: impl FnMut(&str)) -> Result<(), Error> {
        let mut command = Command::new(&self.executable);
        if let Some(profile) = &self.profile {
            command.arg("--profile").arg(profile);
        }
        command.args(["sso", "login", "--use-device-code", "--no-browser"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let (sender, receiver) = std::sync::mpsc::channel();
        let stdout_thread = std::thread::spawn({
            let sender = sender.clone();
            move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = sender.send((false, line));
                }
            }
        });
        let stderr_thread = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send((true, line));
            }
        });
        let mut errors = Vec::new();
        for (is_error, line) in receiver {
            if is_error {
                errors.push(line.clone());
            }
            progress(&line);
        }
        let status = child.wait()?;
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        if !status.success() {
            return Err(command_error(if errors.is_empty() {
                format!("SSO login exited with {status}")
            } else {
                errors.join("\n")
            }));
        }
        Ok(())
    }

    /// List queue URLs visible to this identity in the selected region.
    pub fn list_queues(&self) -> Result<Vec<String>, Error> {
        let bytes = self.output(&["sqs", "list-queues"])?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let output: QueuesOutput = serde_json::from_slice(&bytes)?;
        Ok(output.queue_urls)
    }

    /// Create a standard queue, or a FIFO queue when the name ends in `.fifo`.
    /// FIFO queues enable content-based deduplication so an explicit deduplication
    /// ID remains optional when sending a message.
    pub fn create_queue(&self, name: &str) -> Result<String, Error> {
        let name = name.trim();
        required(name, "queue name")?;
        let stem = name.strip_suffix(".fifo").unwrap_or(name);
        if name.len() > 80
            || stem.is_empty()
            || !stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(Error::InvalidQueueName);
        }
        let mut args = vec!["sqs", "create-queue", "--queue-name", name];
        if name.ends_with(".fifo") {
            args.extend([
                "--attributes",
                "FifoQueue=true,ContentBasedDeduplication=true",
            ]);
        }
        let output: CreateQueueOutput = self.run(&args)?;
        Ok(output.queue_url)
    }

    /// Delete every message in the queue, including messages currently in flight.
    /// SQS may continue removing messages for up to 60 seconds after this succeeds.
    pub fn purge_queue(&self, queue_url: &str) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        self.run_status(&["sqs", "purge-queue", "--queue-url", queue_url])
    }

    /// Permanently delete a queue and all of its messages.
    pub fn delete_queue(&self, queue_url: &str) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        self.run_status(&["sqs", "delete-queue", "--queue-url", queue_url])
    }

    /// Read the three approximate queue depth counters and queue defaults.
    pub fn queue_attributes(&self, queue_url: &str) -> Result<QueueAttributes, Error> {
        required(queue_url, "queue URL")?;
        let output: AttributesOutput = self.run(&[
            "sqs",
            "get-queue-attributes",
            "--queue-url",
            queue_url,
            "--attribute-names",
            "ApproximateNumberOfMessages",
            "ApproximateNumberOfMessagesNotVisible",
            "ApproximateNumberOfMessagesDelayed",
            "DelaySeconds",
            "VisibilityTimeout",
            "ReceiveMessageWaitTimeSeconds",
            "MessageRetentionPeriod",
            "QueueArn",
            "RedrivePolicy",
        ])?;
        let dead_letter_target_arn = output
            .attributes
            .get("RedrivePolicy")
            .map(|raw| serde_json::from_str::<RedrivePolicy>(raw))
            .transpose()?
            .map(|policy| policy.dead_letter_target_arn);
        let count = |key| {
            output
                .attributes
                .get(key)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        Ok(QueueAttributes {
            queue_arn: output.attributes.get("QueueArn").cloned(),
            dead_letter_target_arn,
            available: count("ApproximateNumberOfMessages"),
            in_flight: count("ApproximateNumberOfMessagesNotVisible"),
            delayed: count("ApproximateNumberOfMessagesDelayed"),
            delay_seconds: output
                .attributes
                .get("DelaySeconds")
                .and_then(|value| value.parse().ok()),
            visibility_timeout: output
                .attributes
                .get("VisibilityTimeout")
                .and_then(|value| value.parse().ok()),
            receive_wait_seconds: output
                .attributes
                .get("ReceiveMessageWaitTimeSeconds")
                .and_then(|value| value.parse().ok()),
            message_retention_period: output
                .attributes
                .get("MessageRetentionPeriod")
                .and_then(|value| value.parse().ok()),
        })
    }

    /// Resolve an SQS queue ARN to its URL, including queues in another account.
    pub fn queue_url_for_arn(&self, arn: &str) -> Result<String, Error> {
        let parts = arn.splitn(6, ':').collect::<Vec<_>>();
        if parts.len() != 6
            || parts[0] != "arn"
            || parts[1].is_empty()
            || parts[2] != "sqs"
            || parts[3].is_empty()
            || parts[4].len() != 12
            || !parts[4].bytes().all(|byte| byte.is_ascii_digit())
            || parts[5].is_empty()
        {
            return Err(Error::InvalidQueueArn);
        }
        let output: QueueUrlOutput = self.run(&[
            "sqs",
            "get-queue-url",
            "--queue-name",
            parts[5],
            "--queue-owner-aws-account-id",
            parts[4],
        ])?;
        Ok(output.queue_url)
    }

    /// Start an asynchronous SQS move task from a DLQ to the specified queue.
    /// SQS moves all available messages in the DLQ, so callers must confirm the destination.
    pub fn start_message_move_task(
        &self,
        source_arn: &str,
        destination_arn: &str,
    ) -> Result<String, Error> {
        required(source_arn, "DLQ ARN")?;
        required(destination_arn, "destination queue ARN")?;
        let output: MoveTaskOutput = self.run(&[
            "sqs",
            "start-message-move-task",
            "--source-arn",
            source_arn,
            "--destination-arn",
            destination_arn,
        ])?;
        Ok(output.task_handle)
    }

    /// Set the queue's default delivery delay to 0–900 seconds.
    /// On FIFO queues, changing this setting also affects messages already queued.
    pub fn set_queue_delay(&self, queue_url: &str, seconds: u32) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        if seconds > 900 {
            return Err(Error::InvalidQueueDelay);
        }
        let attribute = format!("DelaySeconds={seconds}");
        self.run_status(&[
            "sqs",
            "set-queue-attributes",
            "--queue-url",
            queue_url,
            "--attributes",
            &attribute,
        ])
    }

    /// Set the queue's default visibility timeout to 0–43,200 seconds.
    /// SQS may take up to 60 seconds to propagate the new setting.
    pub fn set_queue_visibility_timeout(&self, queue_url: &str, seconds: u32) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        if seconds > 43_200 {
            return Err(Error::InvalidQueueVisibilityTimeout);
        }
        let attribute = format!("VisibilityTimeout={seconds}");
        self.run_status(&[
            "sqs",
            "set-queue-attributes",
            "--queue-url",
            queue_url,
            "--attributes",
            &attribute,
        ])
    }

    /// Set the queue's default receive wait to 0–20 seconds.
    /// SQS may take up to 60 seconds to propagate the new setting.
    pub fn set_queue_receive_wait(&self, queue_url: &str, seconds: u8) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        if seconds > 20 {
            return Err(Error::InvalidQueueReceiveWait);
        }
        let attribute = format!("ReceiveMessageWaitTimeSeconds={seconds}");
        self.run_status(&[
            "sqs",
            "set-queue-attributes",
            "--queue-url",
            queue_url,
            "--attributes",
            &attribute,
        ])
    }

    /// Set the queue's message retention period to 60–1,209,600 seconds.
    /// SQS may take up to 15 minutes to apply it. Shortening the period can
    /// expire existing messages whose age exceeds the new value.
    pub fn set_queue_message_retention_period(
        &self,
        queue_url: &str,
        seconds: u32,
    ) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        if !(60..=1_209_600).contains(&seconds) {
            return Err(Error::InvalidQueueMessageRetentionPeriod);
        }
        let attribute = format!("MessageRetentionPeriod={seconds}");
        self.run_status(&[
            "sqs",
            "set-queue-attributes",
            "--queue-url",
            queue_url,
            "--attributes",
            &attribute,
        ])
    }

    /// Receive up to ten messages with a short poll and the queue's visibility timeout.
    /// Receiving hides a message temporarily even if it is not deleted.
    pub fn receive_messages(&self, queue_url: &str) -> Result<Vec<Message>, Error> {
        self.receive_messages_with_wait(queue_url, 0)
    }

    /// Receive up to ten messages, waiting at most `wait_seconds` (0–20) for one to arrive.
    /// Receiving hides returned messages for the queue's visibility timeout.
    pub fn receive_messages_with_wait(
        &self,
        queue_url: &str,
        wait_seconds: u8,
    ) -> Result<Vec<Message>, Error> {
        self.receive_messages_with_options(queue_url, Some(wait_seconds), None)
    }

    /// Receive up to ten messages, optionally overriding the queue's receive wait
    /// and visibility timeout for this receive only. `None` uses the queue default.
    /// Both time values are in seconds.
    pub fn receive_messages_with_options(
        &self,
        queue_url: &str,
        wait_seconds: Option<u8>,
        visibility_timeout: Option<u32>,
    ) -> Result<Vec<Message>, Error> {
        self.receive_messages_with_settings(queue_url, 10, wait_seconds, visibility_timeout)
    }

    /// Receive at most `count` messages (1–10) with optional per-request wait
    /// and visibility overrides. `None` uses the corresponding queue default.
    /// Wait and visibility values are in seconds.
    pub fn receive_messages_with_settings(
        &self,
        queue_url: &str,
        count: u8,
        wait_seconds: Option<u8>,
        visibility_timeout: Option<u32>,
    ) -> Result<Vec<Message>, Error> {
        required(queue_url, "queue URL")?;
        if !(1..=10).contains(&count) {
            return Err(Error::InvalidReceiveCount);
        }
        if wait_seconds.is_some_and(|seconds| seconds > 20) {
            return Err(Error::InvalidReceiveWait);
        }
        if visibility_timeout.is_some_and(|seconds| seconds > 43_200) {
            return Err(Error::InvalidVisibilityTimeout);
        }
        let wait = wait_seconds.map(|seconds| seconds.to_string());
        let visibility = visibility_timeout.map(|seconds| seconds.to_string());
        let count = count.to_string();
        let mut args = vec![
            "sqs",
            "receive-message",
            "--queue-url",
            queue_url,
            "--max-number-of-messages",
            &count,
            "--message-system-attribute-names",
            "ApproximateReceiveCount",
            "SentTimestamp",
            "MessageGroupId",
            "--message-attribute-names",
            "All",
        ];
        if let Some(ref seconds) = wait {
            args.extend(["--wait-time-seconds", seconds.as_str()]);
        }
        if let Some(ref seconds) = visibility {
            args.extend(["--visibility-timeout", seconds.as_str()]);
        }
        let bytes = self.output(&args)?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let output: MessagesOutput = serde_json::from_slice(&bytes)?;
        Ok(output
            .messages
            .into_iter()
            .map(|message| Message {
                receive_count: message
                    .attributes
                    .get("ApproximateReceiveCount")
                    .and_then(|value| value.parse().ok()),
                sent_at_millis: message
                    .attributes
                    .get("SentTimestamp")
                    .and_then(|value| value.parse().ok()),
                group_id: message.attributes.get("MessageGroupId").cloned(),
                message_attributes: message.message_attributes,
                id: message.message_id,
                body: message.body,
                receipt_handle: message.receipt_handle,
            })
            .collect())
    }

    /// Send a message. FIFO queues require a group ID; a deduplication ID is
    /// optional when content-based deduplication is enabled on the queue.
    pub fn send_message(
        &self,
        queue_url: &str,
        body: &str,
        group_id: Option<&str>,
        deduplication_id: Option<&str>,
    ) -> Result<(), Error> {
        self.send_message_with_delay(queue_url, body, group_id, deduplication_id, None)
    }

    /// Send a message with an optional delay in seconds. Omitting the delay
    /// leaves the queue default in effect; FIFO queues cannot override it.
    pub fn send_message_with_delay(
        &self,
        queue_url: &str,
        body: &str,
        group_id: Option<&str>,
        deduplication_id: Option<&str>,
        delay_seconds: Option<u32>,
    ) -> Result<(), Error> {
        self.send_message_with_attributes(
            queue_url,
            body,
            group_id,
            deduplication_id,
            delay_seconds,
            &BTreeMap::new(),
        )
    }

    /// Send a message with optional delay and String custom attributes.
    /// Attribute names and nonempty values are validated before invoking the CLI.
    pub fn send_message_with_attributes(
        &self,
        queue_url: &str,
        body: &str,
        group_id: Option<&str>,
        deduplication_id: Option<&str>,
        delay_seconds: Option<u32>,
        attributes: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        required(body, "message body")?;
        validate_message_attributes(attributes)?;
        if queue_url.ends_with(".fifo") && group_id.is_none_or(|id| id.trim().is_empty()) {
            return Err(Error::Missing("FIFO message group ID"));
        }
        if delay_seconds.is_some_and(|seconds| seconds > 900) {
            return Err(Error::InvalidSendDelay);
        }
        if queue_url.ends_with(".fifo") && delay_seconds.is_some() {
            return Err(Error::FifoMessageDelay);
        }
        let mut args = vec![
            "sqs",
            "send-message",
            "--queue-url",
            queue_url,
            "--message-body",
            body,
        ];
        if let Some(id) = group_id.filter(|id| !id.trim().is_empty()) {
            args.extend(["--message-group-id", id]);
        }
        if let Some(id) = deduplication_id.filter(|id| !id.trim().is_empty()) {
            args.extend(["--message-deduplication-id", id]);
        }
        let delay = delay_seconds.map(|seconds| seconds.to_string());
        if let Some(ref seconds) = delay {
            args.extend(["--delay-seconds", seconds.as_str()]);
        }
        let encoded_attributes = (!attributes.is_empty()).then(|| {
            let values = attributes
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        serde_json::json!({"DataType": "String", "StringValue": value}),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            serde_json::to_string(&values).expect("string attributes serialize to JSON")
        });
        if let Some(ref encoded) = encoded_attributes {
            args.extend(["--message-attributes", encoded.as_str()]);
        }
        let _: serde_json::Value = self.run(&args)?;
        Ok(())
    }

    /// Delete the message addressed by this receive attempt's receipt handle.
    pub fn delete_message(&self, queue_url: &str, receipt_handle: &str) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        required(receipt_handle, "receipt handle")?;
        self.run_status(&[
            "sqs",
            "delete-message",
            "--queue-url",
            queue_url,
            "--receipt-handle",
            receipt_handle,
        ])
    }

    /// Make a received message available again immediately. SQS may redeliver it
    /// to another consumer as soon as the visibility timeout reaches zero.
    pub fn release_message(&self, queue_url: &str, receipt_handle: &str) -> Result<(), Error> {
        self.change_message_visibility(queue_url, receipt_handle, 0)
    }

    /// Set a received message's visibility timeout from this call onward.
    /// This affects only this receive attempt, not the queue default.
    pub fn change_message_visibility(
        &self,
        queue_url: &str,
        receipt_handle: &str,
        timeout_seconds: u32,
    ) -> Result<(), Error> {
        required(queue_url, "queue URL")?;
        required(receipt_handle, "receipt handle")?;
        if timeout_seconds > 43_200 {
            return Err(Error::InvalidVisibilityTimeout);
        }
        self.run_status(&[
            "sqs",
            "change-message-visibility",
            "--queue-url",
            queue_url,
            "--receipt-handle",
            receipt_handle,
            "--visibility-timeout",
            &timeout_seconds.to_string(),
        ])
    }

    fn run<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.output(args)?)?)
    }

    fn run_status(&self, args: &[&str]) -> Result<(), Error> {
        self.output(args)?;
        Ok(())
    }

    fn output(&self, args: &[&str]) -> Result<Vec<u8>, Error> {
        if self.require_sign_in {
            return Err(Error::Missing("AWS sign-in"));
        }
        if let Some(native) = &self.native {
            return native.output(args);
        }
        let mut command = Command::new(&self.executable);
        if let Some(home) = &self.console_home {
            self.console_environment(&mut command, home);
        }
        command.arg("--no-cli-pager").arg("--output").arg("json");
        if let Some(profile) = &self.profile {
            command.arg("--profile").arg(profile);
        }
        if let Some(region) = &self.region {
            command.arg("--region").arg(region);
        }
        let output = command.args(args).output()?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(command_error(error.trim().to_string()));
        }
        Ok(output.stdout)
    }
}

impl Default for AwsCli {
    fn default() -> Self {
        Self::new()
    }
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn validate_region(region: &str) -> Result<(), Error> {
    if region.is_empty()
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(Error::InvalidEndpoint("AWS region"));
    }
    Ok(())
}

fn command_error(message: String) -> Error {
    if message.contains("Missing the following required SSO configuration values")
        || (message.contains("sso_start_url") && message.contains("sso_region"))
    {
        Error::SsoNotConfigured
    } else {
        Error::Command(message)
    }
}

fn required(value: &str, name: &'static str) -> Result<(), Error> {
    if value.trim().is_empty() {
        return Err(Error::Missing(name));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn missing_sso_configuration_is_actionable() {
        assert!(matches!(
            command_error("Missing the following required SSO configuration values: sso_start_url, sso_region".into()),
            Error::SsoNotConfigured
        ));
        assert!(matches!(
            command_error("Access denied".into()),
            Error::Command(message) if message == "Access denied"
        ));
    }

    #[test]
    fn sso_setup_command_targets_selected_profile_without_shell_expansion() {
        assert_eq!(sso_setup_command(""), "aws configure sso");
        assert_eq!(
            sso_setup_command(" work's `account` "),
            "aws configure sso --profile 'work'\\''s `account`'"
        );
    }

    #[test]
    fn extracts_device_authorization_instructions() {
        assert_eq!(
            sso_device_url("Visit https://device.sso.us-east-1.amazonaws.com/."),
            Some("https://device.sso.us-east-1.amazonaws.com/".to_string())
        );
        assert_eq!(
            sso_device_code("Then enter code: ABCD-EFGH"),
            Some("ABCD-EFGH".to_string())
        );
        assert_eq!(sso_device_code("SSO login failed"), None);
    }
    use std::os::unix::fs::PermissionsExt;

    fn cli(response: &str, log: &std::path::Path) -> AwsCli {
        let script = log.with_extension("sh");
        let content = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s' '{}'\n",
            log.display(),
            response
        );
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        AwsCli::new().with_executable(script)
    }

    #[test]
    fn console_login_replaces_previous_session_and_isolates_sqs_requests() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("aws-console");
        std::fs::create_dir(&home).unwrap();
        let config = home.join("config");
        std::fs::write(
            &config,
            "[profile ginka-console]\nregion = us-east-1\nlogin_session = previous-account\n",
        )
        .unwrap();
        let credentials = home.join("credentials");
        std::fs::write(
            &credentials,
            "[ginka-console]\naws_access_key_id = old-key\n",
        )
        .unwrap();
        let script = dir.path().join("aws.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AWS_CONFIG_FILE.args\"\nprintf '%s\\n' \"$AWS_CONFIG_FILE\" \"$AWS_SHARED_CREDENTIALS_FILE\" \"$AWS_LOGIN_CACHE_DIRECTORY\" > \"$AWS_CONFIG_FILE.env\"\nprintf '{}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = AwsCli::console("ap-northeast-1", &home)
            .unwrap()
            .with_executable(script);

        client.console_login().unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[profile ginka-console]\nregion = ap-northeast-1\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.args")).unwrap(),
            "login\n--profile\nginka-console\n--region\nap-northeast-1\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.env")).unwrap(),
            format!(
                "{}\n{}\n{}\n",
                config.display(),
                home.join("credentials").display(),
                home.join("login-cache").display()
            )
        );
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&credentials).unwrap(), "");
        assert_eq!(
            std::fs::metadata(&credentials)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        assert!(client.list_queues().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(home.join("config.args")).unwrap(),
            "--no-cli-pager\n--output\njson\n--profile\nginka-console\n--region\nap-northeast-1\nsqs\nlist-queues\n"
        );
    }

    #[test]
    fn empty_queue_list_and_receive_are_valid() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        assert!(cli("{}", &log).list_queues().unwrap().is_empty());
        assert!(cli("{}", &log).receive_messages("url").unwrap().is_empty());
        assert!(cli("", &log).list_queues().unwrap().is_empty());
        assert!(cli("", &log).receive_messages("url").unwrap().is_empty());
    }

    #[test]
    fn profile_region_and_fifo_arguments_are_separate() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log)
            .with_profile(" work ")
            .with_region(" ap-northeast-1 ");
        client
            .send_message(
                "https://example/queue.fifo",
                "hello world",
                Some("group"),
                Some("dedup"),
            )
            .unwrap();
        let args = std::fs::read_to_string(log).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "--profile",
                "work",
                "--region",
                "ap-northeast-1",
                "sqs",
                "send-message",
                "--queue-url",
                "https://example/queue.fifo",
                "--message-body",
                "hello world",
                "--message-group-id",
                "group",
                "--message-deduplication-id",
                "dedup",
            ]
        );
    }

    #[test]
    fn fifo_requires_group_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let error = cli("{}", &log)
            .send_message("https://example/queue.fifo", "body", None, None)
            .unwrap_err();
        assert!(matches!(error, Error::Missing("FIFO message group ID")));
        assert!(!log.exists());
    }

    #[test]
    fn standard_send_delay_is_optional_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        for seconds in [0, 900] {
            client
                .send_message_with_delay("https://example/queue", "body", None, None, Some(seconds))
                .unwrap();
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(
                args.lines()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|pair| { pair == ["--delay-seconds", seconds.to_string().as_str()] })
            );
        }
        client
            .send_message_with_delay("https://example/queue", "body", None, None, None)
            .unwrap();
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(!args.lines().any(|arg| arg == "--delay-seconds"));
        std::fs::remove_file(&log).unwrap();
        let error = client
            .send_message_with_delay("https://example/queue", "body", None, None, Some(901))
            .unwrap_err();
        assert!(matches!(error, Error::InvalidSendDelay));
        assert!(!log.exists());
    }

    #[test]
    fn fifo_rejects_per_message_delay_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let error = cli("{}", &log)
            .send_message_with_delay(
                "https://example/queue.fifo",
                "body",
                Some("group"),
                None,
                Some(0),
            )
            .unwrap_err();
        assert!(matches!(error, Error::FifoMessageDelay));
        assert!(!log.exists());
    }

    #[test]
    fn login_uses_selected_profile() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        cli("{}", &log).with_profile("work").sso_login().unwrap();
        assert_eq!(
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--profile",
                "work",
                "sso",
                "login",
                "--use-device-code",
                "--no-browser"
            ]
        );
    }

    #[test]
    fn login_streams_device_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let mut lines = Vec::new();
        cli("https://device.sso.example.com/\nABCD-EFGH\n", &log)
            .sso_login_with_progress(|line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["https://device.sso.example.com/", "ABCD-EFGH"]);
    }

    #[test]
    fn parses_messages_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let messages = cli(
            r#"{"Messages":[{"MessageId":"m","Body":"hello","ReceiptHandle":"r"}]}"#,
            &log,
        )
        .receive_messages("url")
        .unwrap();
        assert_eq!(messages[0].receipt_handle, "r");
        assert_eq!(messages[0].receive_count, None);
        assert_eq!(messages[0].sent_at_millis, None);
        assert_eq!(messages[0].group_id, None);
        let counts = cli(r#"{"Attributes":{"ApproximateNumberOfMessages":"3","ApproximateNumberOfMessagesNotVisible":"2"}}"#, &log)
            .queue_attributes("url").unwrap();
        assert_eq!(
            counts,
            QueueAttributes {
                queue_arn: None,
                dead_letter_target_arn: None,
                available: 3,
                in_flight: 2,
                delayed: 0,
                delay_seconds: None,
                visibility_timeout: None,
                receive_wait_seconds: None,
                message_retention_period: None,
            }
        );
    }

    #[test]
    fn reads_dead_letter_queue_from_redrive_policy() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let attrs = cli(
            r#"{"Attributes":{"QueueArn":"arn:aws:sqs:ap-northeast-1:123456789012:orders","RedrivePolicy":"{\"deadLetterTargetArn\":\"arn:aws:sqs:ap-northeast-1:123456789012:orders-dlq\",\"maxReceiveCount\":\"5\"}"}}"#,
            &log,
        )
        .queue_attributes("url")
        .unwrap();
        assert_eq!(
            attrs.queue_arn.as_deref(),
            Some("arn:aws:sqs:ap-northeast-1:123456789012:orders")
        );
        assert_eq!(
            attrs.dead_letter_target_arn.as_deref(),
            Some("arn:aws:sqs:ap-northeast-1:123456789012:orders-dlq")
        );
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.lines().any(|arg| arg == "QueueArn"));
        assert!(args.lines().any(|arg| arg == "RedrivePolicy"));
    }

    #[test]
    fn resolves_dead_letter_queue_url_from_arn() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let url = cli(
            r#"{"QueueUrl":"https://sqs.ap-northeast-1.amazonaws.com/123456789012/orders-dlq"}"#,
            &log,
        )
        .queue_url_for_arn("arn:aws:sqs:ap-northeast-1:123456789012:orders-dlq")
        .unwrap();
        assert!(url.ends_with("/orders-dlq"));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.contains("get-queue-url\n--queue-name\norders-dlq\n"));
        assert!(args.contains("--queue-owner-aws-account-id\n123456789012\n"));
    }

    #[test]
    fn redrive_targets_the_explicit_source_queue() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let handle = cli(r#"{"TaskHandle":"move-123"}"#, &log)
            .start_message_move_task(
                "arn:aws:sqs:ap-northeast-1:123456789012:orders-dlq",
                "arn:aws:sqs:ap-northeast-1:123456789012:orders",
            )
            .unwrap();
        assert_eq!(handle, "move-123");
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.contains("start-message-move-task\n"));
        assert!(
            args.contains("--source-arn\narn:aws:sqs:ap-northeast-1:123456789012:orders-dlq\n")
        );
        assert!(
            args.contains("--destination-arn\narn:aws:sqs:ap-northeast-1:123456789012:orders\n")
        );
    }

    #[test]
    fn receives_selected_system_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let messages = cli(
            r#"{"Messages":[{"MessageId":"m","Body":"hello","ReceiptHandle":"r","Attributes":{"ApproximateReceiveCount":"3","SentTimestamp":"1720000000123","MessageGroupId":"orders"}}]}"#,
            &log,
        )
        .receive_messages("url")
        .unwrap();
        assert_eq!(messages[0].receive_count, Some(3));
        assert_eq!(messages[0].sent_at_millis, Some(1_720_000_000_123));
        assert_eq!(messages[0].group_id.as_deref(), Some("orders"));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.contains("--message-system-attribute-names\n"));
        for name in ["ApproximateReceiveCount", "SentTimestamp", "MessageGroupId"] {
            assert!(args.lines().any(|arg| arg == name));
        }
    }

    #[test]
    fn receives_custom_message_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let messages = cli(
            r#"{"Messages":[{"MessageId":"m","Body":"hello","ReceiptHandle":"r","MessageAttributes":{"order.id":{"DataType":"String","StringValue":"42"},"attempt":{"DataType":"Number","StringValue":"3"},"blob":{"DataType":"Binary","BinaryValue":"AQI="}}}]}"#,
            &log,
        )
        .receive_messages("url")
        .unwrap();
        assert_eq!(
            messages[0].message_attributes["order.id"]
                .string_value
                .as_deref(),
            Some("42")
        );
        assert_eq!(
            messages[0].message_attributes["attempt"].data_type,
            "Number"
        );
        assert_eq!(
            messages[0].message_attributes["blob"]
                .binary_value
                .as_deref(),
            Some("AQI=")
        );
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.contains("--message-attribute-names\nAll\n"));
    }

    #[test]
    fn sends_validated_string_message_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        let attrs = parse_string_message_attributes(r#"{"order.id":"42","source":"web"}"#).unwrap();
        client
            .send_message_with_attributes("url", "body", None, None, None, &attrs)
            .unwrap();
        let args = std::fs::read_to_string(&log).unwrap();
        let args = args.lines().collect::<Vec<_>>();
        let index = args
            .iter()
            .position(|arg| *arg == "--message-attributes")
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(args[index + 1]).unwrap();
        assert_eq!(
            payload["order.id"],
            serde_json::json!({"DataType":"String","StringValue":"42"})
        );
        assert_eq!(payload["source"]["StringValue"], "web");
    }

    #[test]
    fn rejects_invalid_string_message_attributes_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        for input in [
            "[]",
            r#"{"a":1}"#,
            r#"{"AWS.foo":"x"}"#,
            r#"{"a..b":"x"}"#,
            r#"{"a":""}"#,
        ] {
            assert!(parse_string_message_attributes(input).is_err(), "{input}");
        }
        let attrs = (0..11)
            .map(|i| (format!("key{i}"), "value".to_string()))
            .collect();
        assert!(matches!(
            client.send_message_with_attributes("url", "body", None, None, None, &attrs),
            Err(Error::TooManyMessageAttributes)
        ));
        assert!(!log.exists());
    }

    #[test]
    fn reads_queue_delay_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let attrs = cli(r#"{"Attributes":{"DelaySeconds":"45"}}"#, &log)
            .queue_attributes("url")
            .unwrap();
        assert_eq!(attrs.delay_seconds, Some(45));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.lines().any(|arg| arg == "DelaySeconds"));
    }

    #[test]
    fn reads_queue_visibility_timeout_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let attrs = cli(r#"{"Attributes":{"VisibilityTimeout":"30"}}"#, &log)
            .queue_attributes("url")
            .unwrap();
        assert_eq!(attrs.visibility_timeout, Some(30));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.lines().any(|arg| arg == "VisibilityTimeout"));
    }

    #[test]
    fn reads_queue_receive_wait_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let attrs = cli(
            r#"{"Attributes":{"ReceiveMessageWaitTimeSeconds":"12"}}"#,
            &log,
        )
        .queue_attributes("url")
        .unwrap();
        assert_eq!(attrs.receive_wait_seconds, Some(12));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(
            args.lines()
                .any(|arg| arg == "ReceiveMessageWaitTimeSeconds")
        );
    }

    #[test]
    fn reads_queue_message_retention_period_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let attrs = cli(
            r#"{"Attributes":{"MessageRetentionPeriod":"345600"}}"#,
            &log,
        )
        .queue_attributes("url")
        .unwrap();
        assert_eq!(attrs.message_retention_period, Some(345_600));
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.lines().any(|arg| arg == "MessageRetentionPeriod"));
    }

    #[test]
    fn changes_queue_message_retention_period_with_selected_identity_and_validates_range() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log)
            .with_profile("work")
            .with_region("ap-northeast-1");
        for seconds in [60, 1_209_600] {
            client
                .set_queue_message_retention_period("url", seconds)
                .unwrap();
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.ends_with(&format!("--attributes\nMessageRetentionPeriod={seconds}\n")));
            assert!(args.contains("--profile\nwork\n--region\nap-northeast-1\n"));
        }
        std::fs::remove_file(&log).unwrap();
        for seconds in [0, 59, 1_209_601] {
            assert!(matches!(
                client.set_queue_message_retention_period("url", seconds),
                Err(Error::InvalidQueueMessageRetentionPeriod)
            ));
        }
        assert!(matches!(
            client.set_queue_message_retention_period("", 60),
            Err(Error::Missing("queue URL"))
        ));
        assert!(!log.exists());
    }

    #[test]
    fn changes_queue_receive_wait_with_selected_identity_and_validates_range() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log)
            .with_profile("work")
            .with_region("ap-northeast-1");
        for seconds in [0, 20] {
            client.set_queue_receive_wait("url", seconds).unwrap();
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.ends_with(&format!(
                "--attributes\nReceiveMessageWaitTimeSeconds={seconds}\n"
            )));
            assert!(args.contains("--profile\nwork\n--region\nap-northeast-1\n"));
        }
        std::fs::remove_file(&log).unwrap();
        assert!(matches!(
            client.set_queue_receive_wait("url", 21),
            Err(Error::InvalidQueueReceiveWait)
        ));
        assert!(matches!(
            client.set_queue_receive_wait("", 0),
            Err(Error::Missing("queue URL"))
        ));
        assert!(!log.exists());
    }

    #[test]
    fn receive_can_use_queue_default_wait() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        cli("{}", &log)
            .receive_messages_with_options("url", None, Some(0))
            .unwrap();
        let args = std::fs::read_to_string(log).unwrap();
        assert!(!args.contains("--wait-time-seconds"));
        assert!(args.ends_with("--visibility-timeout\n0\n"));
    }

    #[test]
    fn changes_queue_visibility_timeout_with_selected_identity_and_validates_range() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log)
            .with_profile("work")
            .with_region("ap-northeast-1");
        client.set_queue_visibility_timeout("url", 43_200).unwrap();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "--profile",
                "work",
                "--region",
                "ap-northeast-1",
                "sqs",
                "set-queue-attributes",
                "--queue-url",
                "url",
                "--attributes",
                "VisibilityTimeout=43200",
            ]
        );
        assert!(matches!(
            client.set_queue_visibility_timeout("url", 43_201),
            Err(Error::InvalidQueueVisibilityTimeout)
        ));
        assert!(matches!(
            client.set_queue_visibility_timeout("", 0),
            Err(Error::Missing("queue URL"))
        ));
    }

    #[test]
    fn changes_queue_delay_with_selected_identity_and_validates_range() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log)
            .with_profile("work")
            .with_region("ap-northeast-1");
        client.set_queue_delay("url", 900).unwrap();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "--profile",
                "work",
                "--region",
                "ap-northeast-1",
                "sqs",
                "set-queue-attributes",
                "--queue-url",
                "url",
                "--attributes",
                "DelaySeconds=900",
            ]
        );
        assert!(matches!(
            client.set_queue_delay("url", 901),
            Err(Error::InvalidQueueDelay)
        ));
        assert!(matches!(
            client.set_queue_delay("", 0),
            Err(Error::Missing("queue URL"))
        ));
    }

    #[test]
    fn receive_wait_is_passed_to_cli_and_rejects_out_of_range_values() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        assert!(
            client
                .receive_messages_with_wait("url", 20)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "sqs",
                "receive-message",
                "--queue-url",
                "url",
                "--max-number-of-messages",
                "10",
                "--message-system-attribute-names",
                "ApproximateReceiveCount",
                "SentTimestamp",
                "MessageGroupId",
                "--message-attribute-names",
                "All",
                "--wait-time-seconds",
                "20",
            ]
        );
        std::fs::remove_file(&log).unwrap();
        assert!(matches!(
            client.receive_messages_with_wait("url", 21),
            Err(Error::InvalidReceiveWait)
        ));
        assert!(!log.exists());
    }

    #[test]
    fn receive_visibility_timeout_is_optional_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);

        client
            .receive_messages_with_options("url", Some(0), None)
            .unwrap();
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(!args.contains("--visibility-timeout"));

        for seconds in [0, 43_200] {
            client
                .receive_messages_with_options("url", Some(20), Some(seconds))
                .unwrap();
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.contains("--wait-time-seconds\n20\n"));
            assert!(args.ends_with(&format!("--visibility-timeout\n{seconds}\n")));
        }

        std::fs::remove_file(&log).unwrap();
        assert!(matches!(
            client.receive_messages_with_options("url", Some(0), Some(43_201)),
            Err(Error::InvalidVisibilityTimeout)
        ));
        assert!(!log.exists());
    }

    #[test]
    fn receive_count_is_forwarded_and_validated_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        for count in [1, 10] {
            client
                .receive_messages_with_settings("url", count, Some(0), None)
                .unwrap();
            let args = std::fs::read_to_string(&log).unwrap();
            assert!(args.contains(&format!("--max-number-of-messages\n{count}\n")));
        }
        std::fs::remove_file(&log).unwrap();
        for count in [0, 11] {
            assert!(matches!(
                client.receive_messages_with_settings("url", count, Some(0), None),
                Err(Error::InvalidReceiveCount)
            ));
            assert!(!log.exists());
        }
    }

    #[test]
    fn delete_accepts_empty_success_output() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        cli("", &log).delete_message("url", "receipt").unwrap();
        let args = std::fs::read_to_string(log).unwrap();
        assert!(args.contains("delete-message"));
        assert!(args.contains("receipt"));
    }

    #[test]
    fn create_standard_and_fifo_queues_with_separate_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli(r#"{"QueueUrl":"https://example/jobs"}"#, &log);
        assert_eq!(client.create_queue("jobs").unwrap(), "https://example/jobs");
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "sqs",
                "create-queue",
                "--queue-name",
                "jobs"
            ]
        );
        assert_eq!(
            client.create_queue("jobs.fifo").unwrap(),
            "https://example/jobs"
        );
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "sqs",
                "create-queue",
                "--queue-name",
                "jobs.fifo",
                "--attributes",
                "FifoQueue=true,ContentBasedDeduplication=true"
            ]
        );
    }

    #[test]
    fn rejects_invalid_queue_name_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("{}", &log);
        assert!(matches!(
            client.create_queue(" "),
            Err(Error::Missing("queue name"))
        ));
        for name in ["bad name", "a/b", "foo.fifo.fifo", "a".repeat(81).as_str()] {
            assert!(matches!(
                client.create_queue(name),
                Err(Error::InvalidQueueName)
            ));
        }
        assert!(!log.exists());
    }

    #[test]
    fn release_message_sets_visibility_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log);
        client.release_message("queue-url", "receipt").unwrap();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "sqs",
                "change-message-visibility",
                "--queue-url",
                "queue-url",
                "--receipt-handle",
                "receipt",
                "--visibility-timeout",
                "0"
            ]
        );
        assert!(matches!(
            client.release_message("queue-url", " "),
            Err(Error::Missing("receipt handle"))
        ));
    }

    #[test]
    fn change_message_visibility_validates_range_and_uses_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log);
        assert!(matches!(
            client.change_message_visibility("queue-url", "receipt", 43_201),
            Err(Error::InvalidVisibilityTimeout)
        ));
        assert!(!log.exists());
        client
            .change_message_visibility("queue-url", "receipt", 43_200)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--no-cli-pager",
                "--output",
                "json",
                "sqs",
                "change-message-visibility",
                "--queue-url",
                "queue-url",
                "--receipt-handle",
                "receipt",
                "--visibility-timeout",
                "43200"
            ]
        );
        assert!(matches!(
            client.change_message_visibility("queue-url", " ", 30),
            Err(Error::Missing("receipt handle"))
        ));
    }

    #[test]
    fn purge_and_delete_queue_use_the_selected_url() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log)
            .with_profile("work")
            .with_region("ap-northeast-1");
        for (operation, expected) in [
            ("purge-queue", "purge-queue"),
            ("delete-queue", "delete-queue"),
        ] {
            match operation {
                "purge-queue" => client.purge_queue("https://example/jobs").unwrap(),
                _ => client.delete_queue("https://example/jobs").unwrap(),
            }
            assert_eq!(
                std::fs::read_to_string(&log)
                    .unwrap()
                    .lines()
                    .collect::<Vec<_>>(),
                [
                    "--no-cli-pager",
                    "--output",
                    "json",
                    "--profile",
                    "work",
                    "--region",
                    "ap-northeast-1",
                    "sqs",
                    expected,
                    "--queue-url",
                    "https://example/jobs"
                ]
            );
        }
    }

    #[test]
    fn purge_and_delete_queue_reject_empty_url_before_invoking_cli() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let client = cli("", &log);
        assert!(matches!(
            client.purge_queue(" "),
            Err(Error::Missing("queue URL"))
        ));
        assert!(matches!(
            client.delete_queue(" "),
            Err(Error::Missing("queue URL"))
        ));
        assert!(!log.exists());
    }
}
