//! Provider setting edits shared by the native settings form.

use ginka_protocol::ProviderKind;
use ginka_protocol::rpc::Request;

/// Turn one executable field into the daemon's atomic update request.
/// An empty field restores the driver's automatic executable discovery.
pub fn program_request(provider: ProviderKind, input: &str) -> Request {
    let program = input.trim();
    Request::UpdateProviderSettings {
        provider,
        enabled: None,
        program: (!program.is_empty()).then(|| program.to_string()),
        clear_program: program.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_an_executable_override_without_changing_provider_availability() {
        assert_eq!(
            program_request(ProviderKind::Codex, "  /opt/agents/codex  "),
            Request::UpdateProviderSettings {
                provider: ProviderKind::Codex,
                enabled: None,
                program: Some("/opt/agents/codex".into()),
                clear_program: false,
            }
        );
    }

    #[test]
    fn empty_field_restores_automatic_discovery() {
        assert_eq!(
            program_request(ProviderKind::Claude, "  \t  "),
            Request::UpdateProviderSettings {
                provider: ProviderKind::Claude,
                enabled: None,
                program: None,
                clear_program: true,
            }
        );
    }
}
