use super::*;

#[test]
fn classifies_credential_capable_commands_without_forwarding_command_text() {
    let command = vec![
        "/usr/bin/git".to_string(),
        "push".to_string(),
        "https://github.com/private/repository-with-secret-name".to_string(),
    ];

    let classification = classify_sensitive_command(&command).expect("git push is sensitive");

    assert_eq!(
        classification,
        SensitiveCommand {
            kind: "credential",
            action: "use repository credential",
            summary: "Credential-capable Git operation",
            target_service: Some("github.com")
        }
    );
    assert!(!classification.summary.contains("private"));
    assert!(!classification.summary.contains("secret"));
    assert!(
        !classification
            .summary
            .contains("repository-with-secret-name")
    );
}

#[test]
fn derives_requesting_executable_without_forwarding_command_arguments() {
    let command = vec![
        "/usr/bin/git-credential-osxkeychain".to_string(),
        "get".to_string(),
        "secret=must-not-leave-codex".to_string(),
    ];

    assert_eq!(
        command_executable(&command).as_deref(),
        Some("git-credential-osxkeychain")
    );
}

#[test]
fn leaves_non_sensitive_commands_unannounced() {
    let command = vec![
        "git".to_string(),
        "status".to_string(),
        "--porcelain".to_string(),
    ];

    assert_eq!(classify_sensitive_command(&command), None);
}

#[test]
fn classifies_keychain_and_permission_commands() {
    let keychain = vec![
        "security".to_string(),
        "find-generic-password".to_string(),
        "-s".to_string(),
        "github.com".to_string(),
    ];
    let signing = vec!["/usr/bin/codesign".to_string(), "--sign".to_string()];

    assert_eq!(
        classify_sensitive_command(&keychain),
        Some(SensitiveCommand {
            kind: "keychain",
            action: "read Keychain credential",
            summary: "macOS Keychain access",
            target_service: Some("github.com")
        })
    );
    assert_eq!(
        classify_sensitive_command(&signing),
        Some(SensitiveCommand {
            kind: "permission",
            action: "use signing identity",
            summary: "Code-signing identity access",
            target_service: None
        })
    );
}

#[test]
fn parses_only_the_request_id_from_a_macctl_response() {
    let output = br#"{"result":{"notice":{"request_id":"notice-123"}}}"#;

    assert_eq!(parse_request_id(output).as_deref(), Some("notice-123"));
    assert_eq!(parse_request_id(br#"{"result":{"notice":null}}"#), None);
}

#[test]
fn project_labels_use_repository_markers_without_exposing_the_full_path() {
    let cwd = PathUri::from_host_native_path("/Users/example/projects/mac-control/Sources")
        .expect("absolute path");

    assert_eq!(
        project_labels(&cwd),
        ("mac-control".to_string(), Some("mac-control".to_string()))
    );
}
