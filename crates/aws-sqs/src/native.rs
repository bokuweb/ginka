//! Direct IAM Identity Center sign-in and signed SQS JSON requests.

use crate::Error;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use ureq::Agent;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

/// One AWS account assigned to the signed-in Identity Center user.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Stable twelve-digit account identifier.
    pub account_id: String,
    /// Name shown in the account picker.
    pub account_name: String,
}

/// One role assigned to the signed-in user in an account.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    /// Role name to pass to the federation endpoint.
    pub role_name: String,
}

/// Browser instructions for an in-progress device authorization.
#[derive(Clone, Debug)]
pub struct DeviceAuthorization {
    /// URL the user opens to approve sign-in.
    pub verification_uri: String,
    /// Short code entered on the approval page.
    pub user_code: String,
}

/// Short-lived Identity Center bearer token kept only in memory.
#[derive(Clone)]
pub struct SsoSession {
    region: String,
    access_token: String,
}

/// Short-lived role credentials kept only in memory.
#[derive(Clone)]
pub struct RoleCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
    expires_at_millis: i64,
}

/// Direct client for Identity Center OIDC and account assignments.
#[derive(Clone)]
pub struct IdentityCenter {
    agent: Agent,
}

impl Default for IdentityCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentityCenter {
    /// Create a blocking HTTPS client; call it from a background thread.
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build();
        Self {
            agent: Agent::new_with_config(config),
        }
    }

    /// Authorize a public client through the browser and return an in-memory session.
    /// The callback receives the code before polling begins.
    pub fn sign_in(
        &self,
        start_url: &str,
        sso_region: &str,
        mut on_device: impl FnMut(DeviceAuthorization),
    ) -> Result<SsoSession, Error> {
        let start = Url::parse(start_url).map_err(|_| Error::InvalidEndpoint("start URL"))?;
        if start.scheme() != "https" || start.host_str().is_none() || start.username() != "" {
            return Err(Error::InvalidEndpoint("start URL"));
        }
        validate_region(sso_region)?;
        let base = format!("https://oidc.{sso_region}.amazonaws.com");
        let registered = self.post(
            &format!("{base}/client/register"),
            json!({
                "clientName": "ginka-aws", "clientType": "public", "scopes": ["sso:account:access"]
            }),
        )?;
        let client_id = field(&registered, "clientId")?;
        let client_secret = field(&registered, "clientSecret")?;
        let device = self.post(
            &format!("{base}/device_authorization"),
            json!({
                "clientId": client_id, "clientSecret": client_secret, "startUrl": start_url
            }),
        )?;
        let device_code = field(&device, "deviceCode")?;
        on_device(DeviceAuthorization {
            verification_uri: field(&device, "verificationUri")?.to_string(),
            user_code: field(&device, "userCode")?.to_string(),
        });
        let interval = device["interval"].as_u64().unwrap_or(5).max(1);
        let expiry =
            Instant::now() + Duration::from_secs(device["expiresIn"].as_u64().unwrap_or(600));
        let mut delay = interval;
        loop {
            if Instant::now() >= expiry {
                return Err(Error::DeviceExpired);
            }
            std::thread::sleep(Duration::from_secs(delay));
            let (status, answer) = self.post_response(
                &format!("{base}/token"),
                json!({
                    "clientId": client_id, "clientSecret": client_secret,
                    "deviceCode": device_code,
                    "grantType": "urn:ietf:params:oauth:grant-type:device_code"
                }),
            )?;
            if status < 300 {
                return Ok(SsoSession {
                    region: sso_region.to_string(),
                    access_token: field(&answer, "accessToken")?.to_string(),
                });
            }
            match answer["error"].as_str().unwrap_or_default() {
                "authorization_pending" => {}
                "slow_down" => delay = (delay + 5).min(30),
                "expired_token" => return Err(Error::DeviceExpired),
                _ => return Err(api_error(status, &answer)),
            }
        }
    }

    /// Authorize in a browser on this machine using PKCE and a loopback callback.
    /// The callback receives the browser URL after the listener is ready.
    pub fn sign_in_pkce(
        &self,
        start_url: &str,
        sso_region: &str,
        mut on_browser: impl FnMut(String),
    ) -> Result<SsoSession, Error> {
        let start = Url::parse(start_url).map_err(|_| Error::InvalidEndpoint("start URL"))?;
        if start.scheme() != "https" || start.host_str().is_none() || start.username() != "" {
            return Err(Error::InvalidEndpoint("start URL"));
        }
        validate_region(sso_region)?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(Error::CallbackIo)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/oauth/callback",
            listener.local_addr().map_err(Error::CallbackIo)?.port()
        );
        let verifier = random_url_safe()?;
        let state = random_url_safe()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let base = format!("https://oidc.{sso_region}.amazonaws.com");
        let registered = self.post(
            &format!("{base}/client/register"),
            json!({
                "clientName": "ginka-aws", "clientType": "public",
                "grantTypes": ["authorization_code"],
                "redirectUris": ["http://127.0.0.1/oauth/callback"],
                "issuerUrl": start_url,
                "scopes": ["sso:account:access"]
            }),
        )?;
        let client_id = field(&registered, "clientId")?;
        let client_secret = field(&registered, "clientSecret")?;
        let endpoint = field(&registered, "authorizationEndpoint")?;
        let mut url =
            Url::parse(endpoint).map_err(|_| Error::InvalidEndpoint("authorization URL"))?;
        if url.scheme() != "https" || url.host_str().is_none() || url.username() != "" {
            return Err(Error::InvalidEndpoint("authorization URL"));
        }
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("scopes", "sso:account:access");
        on_browser(url.into());
        let code = wait_for_callback(listener, &state)?;
        let token = self.post(
            &format!("{base}/token"),
            json!({
                "clientId": client_id, "clientSecret": client_secret,
                "grantType": "authorization_code", "code": code,
                "codeVerifier": verifier, "redirectUri": redirect_uri
            }),
        )?;
        Ok(SsoSession {
            region: sso_region.to_string(),
            access_token: field(&token, "accessToken")?.to_string(),
        })
    }

    /// List all assigned accounts, following the portal's pagination token.
    pub fn accounts(&self, session: &SsoSession) -> Result<Vec<Account>, Error> {
        let mut accounts = Vec::new();
        let mut next = None;
        loop {
            let page = self.portal(
                session,
                "/assignment/accounts",
                &[("max_result", "100")],
                next.as_deref(),
            )?;
            accounts.extend(serde_json::from_value::<Vec<Account>>(
                page["accountList"].clone(),
            )?);
            next = page["nextToken"]
                .as_str()
                .map(str::to_string)
                .filter(|token| !token.is_empty());
            if next.is_none() {
                return Ok(accounts);
            }
        }
    }

    /// List every role assigned to the selected account.
    pub fn roles(&self, session: &SsoSession, account_id: &str) -> Result<Vec<Role>, Error> {
        validate_account(account_id)?;
        let mut roles = Vec::new();
        let mut next = None;
        loop {
            let page = self.portal(
                session,
                "/assignment/roles",
                &[("account_id", account_id), ("max_result", "100")],
                next.as_deref(),
            )?;
            roles.extend(serde_json::from_value::<Vec<Role>>(
                page["roleList"].clone(),
            )?);
            next = page["nextToken"]
                .as_str()
                .map(str::to_string)
                .filter(|token| !token.is_empty());
            if next.is_none() {
                return Ok(roles);
            }
        }
    }

    /// Exchange a selected account and role for temporary SQS signing credentials.
    pub fn role_credentials(
        &self,
        session: &SsoSession,
        account_id: &str,
        role_name: &str,
    ) -> Result<RoleCredentials, Error> {
        validate_account(account_id)?;
        let page = self.portal(
            session,
            "/federation/credentials",
            &[("account_id", account_id), ("role_name", role_name)],
            None,
        )?;
        let credentials = &page["roleCredentials"];
        Ok(RoleCredentials {
            access_key_id: field(credentials, "accessKeyId")?.to_string(),
            secret_access_key: field(credentials, "secretAccessKey")?.to_string(),
            session_token: field(credentials, "sessionToken")?.to_string(),
            expires_at_millis: credentials["expiration"]
                .as_i64()
                .ok_or_else(|| Error::Api("role credentials omitted expiration".into()))?,
        })
    }

    fn portal(
        &self,
        session: &SsoSession,
        path: &str,
        params: &[(&str, &str)],
        next: Option<&str>,
    ) -> Result<Value, Error> {
        let mut url = Url::parse(&format!(
            "https://portal.sso.{}.amazonaws.com{path}",
            session.region
        ))
        .map_err(|_| Error::InvalidEndpoint("SSO region"))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
            if let Some(next) = next {
                query.append_pair("next_token", next);
            }
        }
        let mut response = self
            .agent
            .get(url.as_str())
            .header("x-amz-sso_bearer_token", &session.access_token)
            .call()
            .map_err(|e| Error::Api(format!("Identity Center request failed: {e}")))?;
        let status = response.status().as_u16();
        let value: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| Error::Api(format!("invalid Identity Center response: {e}")))?;
        if status >= 300 {
            return Err(api_error(status, &value));
        }
        Ok(value)
    }

    fn post(&self, url: &str, body: Value) -> Result<Value, Error> {
        let (status, value) = self.post_response(url, body)?;
        if status >= 300 {
            return Err(api_error(status, &value));
        }
        Ok(value)
    }

    fn post_response(&self, url: &str, body: Value) -> Result<(u16, Value), Error> {
        let mut response = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| Error::Api(format!("Identity Center request failed: {e}")))?;
        let status = response.status().as_u16();
        let value = response
            .body_mut()
            .read_json()
            .map_err(|e| Error::Api(format!("invalid Identity Center response: {e}")))?;
        Ok((status, value))
    }
}

/// Signed SQS JSON transport behind the existing typed queue operations.
#[derive(Clone)]
pub(crate) struct NativeSqs {
    region: String,
    credentials: RoleCredentials,
    agent: Agent,
}

impl NativeSqs {
    pub(crate) fn new(region: String, credentials: RoleCredentials) -> Result<Self, Error> {
        validate_region(&region)?;
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(35)))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            region,
            credentials,
            agent: Agent::new_with_config(config),
        })
    }

    pub(crate) fn output(&self, args: &[&str]) -> Result<Vec<u8>, Error> {
        if args.first() != Some(&"sqs") {
            return Err(Error::Api("unsupported native AWS operation".into()));
        }
        let operation = args
            .get(1)
            .ok_or_else(|| Error::Api("missing SQS operation".into()))?;
        let value = |name| flag(args, name).ok_or_else(|| Error::Api(format!("missing {name}")));
        let (action, mut body) = match *operation {
            "list-queues" => ("ListQueues", json!({"MaxResults": 1000})),
            "create-queue" => {
                let mut body = json!({"QueueName": value("--queue-name")?});
                if value("--queue-name")?.ends_with(".fifo") {
                    body["Attributes"] =
                        json!({"FifoQueue": "true", "ContentBasedDeduplication": "true"});
                }
                ("CreateQueue", body)
            }
            "purge-queue" => ("PurgeQueue", json!({"QueueUrl": value("--queue-url")?})),
            "delete-queue" => ("DeleteQueue", json!({"QueueUrl": value("--queue-url")?})),
            "get-queue-attributes" => (
                "GetQueueAttributes",
                json!({"QueueUrl": value("--queue-url")?, "AttributeNames": ["ApproximateNumberOfMessages", "ApproximateNumberOfMessagesNotVisible", "ApproximateNumberOfMessagesDelayed", "DelaySeconds", "VisibilityTimeout", "ReceiveMessageWaitTimeSeconds", "MessageRetentionPeriod"]}),
            ),
            "set-queue-attributes" => {
                let (name, val) = value("--attributes")?
                    .split_once('=')
                    .ok_or_else(|| Error::Api("invalid queue attribute".into()))?;
                let mut attributes = serde_json::Map::new();
                attributes.insert(name.to_string(), json!(val));
                (
                    "SetQueueAttributes",
                    json!({"QueueUrl": value("--queue-url")?, "Attributes": attributes}),
                )
            }
            "receive-message" => {
                let mut body = json!({"QueueUrl": value("--queue-url")?, "MaxNumberOfMessages": number(value("--max-number-of-messages")?)?, "MessageSystemAttributeNames": ["ApproximateReceiveCount", "SentTimestamp", "MessageGroupId"], "MessageAttributeNames": ["All"]});
                for (flag_name, key) in [
                    ("--wait-time-seconds", "WaitTimeSeconds"),
                    ("--visibility-timeout", "VisibilityTimeout"),
                ] {
                    if let Some(val) = flag(args, flag_name) {
                        body[key] = json!(number(val)?);
                    }
                }
                ("ReceiveMessage", body)
            }
            "send-message" => {
                let mut body = json!({"QueueUrl": value("--queue-url")?, "MessageBody": value("--message-body")?});
                for (flag_name, key) in [
                    ("--message-group-id", "MessageGroupId"),
                    ("--message-deduplication-id", "MessageDeduplicationId"),
                ] {
                    if let Some(val) = flag(args, flag_name) {
                        body[key] = json!(val);
                    }
                }
                if let Some(val) = flag(args, "--delay-seconds") {
                    body["DelaySeconds"] = json!(number(val)?);
                }
                if let Some(val) = flag(args, "--message-attributes") {
                    body["MessageAttributes"] = serde_json::from_str(val)?;
                }
                ("SendMessage", body)
            }
            "delete-message" => (
                "DeleteMessage",
                json!({"QueueUrl": value("--queue-url")?, "ReceiptHandle": value("--receipt-handle")?}),
            ),
            "change-message-visibility" => (
                "ChangeMessageVisibility",
                json!({"QueueUrl": value("--queue-url")?, "ReceiptHandle": value("--receipt-handle")?, "VisibilityTimeout": number(value("--visibility-timeout")?)?}),
            ),
            _ => {
                return Err(Error::Api(format!(
                    "unsupported SQS operation: {operation}"
                )));
            }
        };
        let mut result = self.call(action, &body)?;
        if action == "ListQueues" {
            let mut urls = result["QueueUrls"].as_array().cloned().unwrap_or_default();
            while let Some(token) = result["NextToken"].as_str().filter(|s| !s.is_empty()) {
                body["NextToken"] = json!(token);
                result = self.call(action, &body)?;
                urls.extend(result["QueueUrls"].as_array().cloned().unwrap_or_default());
            }
            result = json!({"QueueUrls": urls});
        }
        Ok(serde_json::to_vec(&result)?)
    }

    fn call(&self, action: &str, body: &Value) -> Result<Value, Error> {
        if Utc::now().timestamp_millis() >= self.credentials.expires_at_millis - 30_000 {
            return Err(Error::CredentialsExpired);
        }
        let host = format!("sqs.{}.amazonaws.com", self.region);
        let target = format!("AmazonSQS.{action}");
        let body = serde_json::to_vec(body)?;
        let now = Utc::now();
        let date = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let authorization = sign(
            &self.credentials,
            &self.region,
            &host,
            &target,
            &amz_date,
            &date,
            &body,
        );
        let mut response = self
            .agent
            .post(&format!("https://{host}/"))
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("X-Amz-Target", &target)
            .header("X-Amz-Date", &amz_date)
            .header("X-Amz-Security-Token", &self.credentials.session_token)
            .header("Authorization", &authorization)
            .send(&body)
            .map_err(|e| Error::Api(format!("SQS request failed: {e}")))?;
        let status = response.status().as_u16();
        let value: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| Error::Api(format!("invalid SQS response: {e}")))?;
        if status >= 300 {
            return Err(api_error(status, &value));
        }
        Ok(value)
    }
}

fn sign(
    credentials: &RoleCredentials,
    region: &str,
    host: &str,
    target: &str,
    amz_date: &str,
    date: &str,
    body: &[u8],
) -> String {
    let headers = format!(
        "content-type:application/x-amz-json-1.0\nhost:{host}\nx-amz-date:{amz_date}\nx-amz-security-token:{}\nx-amz-target:{target}\n",
        credentials.session_token
    );
    let names = "content-type;host;x-amz-date;x-amz-security-token;x-amz-target";
    let canonical = format!("POST\n/\n\n{headers}\n{names}\n{:x}", Sha256::digest(body));
    let scope = format!("{date}/{region}/sqs/aws4_request");
    let signing = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{:x}",
        Sha256::digest(canonical.as_bytes())
    );
    let mut key = format!("AWS4{}", credentials.secret_access_key).into_bytes();
    for part in [date, region, "sqs", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hmac(&key, signing.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
        credentials.access_key_id
    )
}

fn hmac(key: &[u8], input: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(input);
    mac.finalize().into_bytes().to_vec()
}

fn flag<'a>(args: &'a [&str], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| *arg == name)
        .and_then(|index| args.get(index + 1))
        .copied()
}

fn number(input: &str) -> Result<u32, Error> {
    input
        .parse()
        .map_err(|_| Error::Api("invalid numeric SQS argument".into()))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    value[name]
        .as_str()
        .ok_or_else(|| Error::Api(format!("AWS response omitted {name}")))
}

fn api_error(status: u16, value: &Value) -> Error {
    let code = value["__type"]
        .as_str()
        .or_else(|| value["error"].as_str())
        .or_else(|| value["code"].as_str())
        .unwrap_or("unknown error");
    let code = code.rsplit('#').next().unwrap_or(code);
    let message = value["message"]
        .as_str()
        .or_else(|| value["Message"].as_str())
        .unwrap_or("");
    Error::Api(format!("HTTP {status}: {code} {message}"))
}

fn random_url_safe() -> Result<String, Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Api(format!("secure random source failed: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String, Error> {
    listener.set_nonblocking(true).map_err(Error::CallbackIo)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Api(
                "browser sign-in timed out; sign in again".into(),
            ));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(Error::CallbackIo)?;
                let mut request = Vec::with_capacity(1024);
                let mut chunk = [0_u8; 1024];
                while request.len() < 8192 && !request.windows(2).any(|part| part == b"\r\n") {
                    let count = stream.read(&mut chunk).map_err(Error::CallbackIo)?;
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                }
                let line = request
                    .split(|byte| *byte == b'\r')
                    .next()
                    .and_then(|line| std::str::from_utf8(line).ok());
                let Some(line) = line else {
                    reply(&mut stream, false);
                    continue;
                };
                match callback_code(line, expected_state) {
                    Ok(Some(code)) => {
                        reply(&mut stream, true);
                        return Ok(code);
                    }
                    Ok(None) => reply(&mut stream, false),
                    Err(error) => {
                        reply(&mut stream, false);
                        return Err(error);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(Error::CallbackIo(error)),
        }
    }
}

fn callback_code(request_line: &str, expected_state: &str) -> Result<Option<String>, Error> {
    let Some(target) = request_line
        .strip_prefix("GET ")
        .and_then(|line| line.strip_suffix(" HTTP/1.1"))
    else {
        return Ok(None);
    };
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| Error::Api("invalid browser callback".into()))?;
    if url.path() != "/oauth/callback" {
        return Ok(None);
    }
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return Err(Error::Api("browser sign-in state did not match".into()));
    }
    if let Some(error) = params.get("error") {
        return Err(Error::Api(format!("browser sign-in was denied: {error}")));
    }
    params
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .map(Some)
        .ok_or_else(|| Error::Api("browser sign-in did not return a code".into()))
}

fn reply(stream: &mut TcpStream, success: bool) {
    let (status, body) = if success {
        ("200 OK", "Sign-in complete. You can return to the AWS app.")
    } else {
        (
            "400 Bad Request",
            "Invalid sign-in response. Return to the AWS app.",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn validate_region(region: &str) -> Result<(), Error> {
    if region.is_empty()
        || region.len() > 32
        || !region
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(Error::InvalidEndpoint("AWS region"));
    }
    Ok(())
}

fn validate_account(account: &str) -> Result<(), Error> {
    if account.len() != 12 || !account.bytes().all(|c| c.is_ascii_digit()) {
        return Err(Error::InvalidEndpoint("AWS account ID"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_reject_host_injection() {
        assert!(validate_region("us-east-1").is_ok());
        assert!(validate_region("us-east-1.evil.com").is_err());
        assert!(validate_account("123456789012").is_ok());
        assert!(validate_account("12345678901/").is_err());
    }

    #[test]
    fn pkce_callback_requires_matching_state_and_code() {
        assert_eq!(
            callback_code(
                "GET /oauth/callback?state=expected&code=abc HTTP/1.1",
                "expected"
            )
            .unwrap(),
            Some("abc".into())
        );
        assert!(
            callback_code(
                "GET /oauth/callback?state=other&code=abc HTTP/1.1",
                "expected"
            )
            .is_err()
        );
        assert_eq!(
            callback_code("GET /other?state=expected&code=abc HTTP/1.1", "expected").unwrap(),
            None
        );
        assert!(
            callback_code(
                "GET /oauth/callback?state=expected&error=access_denied HTTP/1.1",
                "expected"
            )
            .is_err()
        );
        assert!(callback_code("GET /oauth/callback?state=expected HTTP/1.1", "expected").is_err());
    }

    #[test]
    fn pkce_entropy_and_challenge_are_url_safe() {
        let verifier = random_url_safe().unwrap();
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert_ne!(verifier, random_url_safe().unwrap());
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(b"test")),
            "n4bQgYhMfWWaL-qgxVrQFaO_TxsrC4Is0V1sFbDwCgg"
        );
    }

    #[test]
    fn pkce_loopback_receives_code_and_replies_to_browser() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let callback = std::thread::spawn(move || wait_for_callback(listener, "expected"));
        let mut browser = TcpStream::connect(address).unwrap();
        browser
            .write_all(
                b"GET /oauth/callback?state=expected&code=abc HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        browser.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(callback.join().unwrap().unwrap(), "abc");
    }

    #[test]
    fn signing_covers_payload_and_session_token() {
        let credentials = RoleCredentials {
            access_key_id: "AKID".into(),
            secret_access_key: "secret".into(),
            session_token: "session_token_value".into(),
            expires_at_millis: i64::MAX,
        };
        let first = sign(
            &credentials,
            "us-east-1",
            "sqs.us-east-1.amazonaws.com",
            "AmazonSQS.ListQueues",
            "20261001T000000Z",
            "20261001",
            b"{}",
        );
        let second = sign(
            &credentials,
            "us-east-1",
            "sqs.us-east-1.amazonaws.com",
            "AmazonSQS.ListQueues",
            "20261001T000000Z",
            "20261001",
            b"{\"NextToken\":\"a\"}",
        );
        assert!(first.contains(
            "SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target"
        ));
        assert_ne!(first, second);
        assert!(!first.contains("secret"));
        assert!(!first.contains("session_token_value"));
    }
}
