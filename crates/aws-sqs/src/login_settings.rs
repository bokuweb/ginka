//! Non-secret AWS connection settings for the standalone client.

use serde::{Deserialize, Serialize};

/// Last successful connection, without tokens or AWS credentials.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    /// Public-client IAM Identity Center authorization.
    #[default]
    IdentityCenter,
    /// AWS CLI Console login using an app-owned profile.
    Console,
}

/// Last successful connection, without tokens or AWS credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoginSettings {
    /// Authentication method to offer on the next launch.
    pub method: LoginMethod,
    /// Organization's IAM Identity Center start URL.
    pub start_url: String,
    /// Region hosting the IAM Identity Center instance.
    pub sso_region: String,
    /// Region used for SQS requests.
    pub region: String,
}

impl LoginSettings {
    /// Whether the browser flow has enough connection information to start.
    pub fn is_complete(&self) -> bool {
        !self.region.trim().is_empty()
            && (self.method == LoginMethod::Console
                || (!self.start_url.trim().is_empty() && !self.sso_region.trim().is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_non_secret_connection_fields_are_serialized() {
        let settings = LoginSettings {
            method: LoginMethod::IdentityCenter,
            start_url: "https://example.awsapps.com/start".into(),
            sso_region: "us-east-1".into(),
            region: "ap-northeast-1".into(),
        };
        assert!(settings.is_complete());
        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(
            serde_json::from_value::<LoginSettings>(value).unwrap(),
            settings
        );
        assert!(
            !LoginSettings {
                region: " ".into(),
                ..settings
            }
            .is_complete()
        );
        let console = LoginSettings {
            method: LoginMethod::Console,
            region: "ap-northeast-1".into(),
            ..LoginSettings::default()
        };
        assert!(console.is_complete());
        assert_eq!(
            serde_json::from_str::<LoginSettings>(r#"{"region":"ap-northeast-1"}"#)
                .unwrap()
                .method,
            LoginMethod::IdentityCenter
        );
    }
}
