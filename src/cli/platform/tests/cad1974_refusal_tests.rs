//! CAD-1974 independent refusal check (non-author).
use super::super::*;

#[test]
fn cad1974_refusal_missing_both_flags_names_connection_first() -> Result<()> {
    let error = connected_read_flags(
        "marketing_report",
        ToolFacts {
            direct: true,
            connected_read: true,
        },
        None,
        None,
    )
    .expect_err("a connected read without either flag must refuse");
    let message = error.to_string();
    assert!(message.contains("--connection"), "{message}");
    assert!(
        !message.contains("--max-charge"),
        "the first missing fact must be --connection: {message}"
    );
    Ok(())
}

#[test]
fn cad1974_refusal_connection_alone_names_missing_max_charge() -> Result<()> {
    let error = connected_read_flags(
        "marketing_report",
        ToolFacts {
            direct: true,
            connected_read: true,
        },
        Some("con_marketing_read_1974".to_owned()),
        None,
    )
    .expect_err("a connection alone must not admit a connected read");
    let message = error.to_string();
    assert!(message.contains("--max-charge"), "{message}");
    assert!(
        !message.contains("--connection"),
        "an existing connection must not be reported as missing: {message}"
    );
    Ok(())
}

#[test]
fn cad1974_refusal_plain_tool_rejects_max_charge_alone() -> Result<()> {
    let error = connected_read_flags(
        "plain_tool",
        ToolFacts {
            direct: true,
            connected_read: false,
        },
        None,
        Some(101),
    )
    .expect_err("a plain tool must refuse even an isolated --max-charge flag");
    let message = error.to_string();
    assert!(message.contains("--max-charge"), "{message}");
    Ok(())
}

#[test]
fn cad1974_refusal_connected_read_preserves_connection_and_minor_units() -> Result<()> {
    for max_charge in [101, 4_294_967_397, u64::MAX] {
        let flags = connected_read_flags(
            "marketing_report",
            ToolFacts {
                direct: true,
                connected_read: true,
            },
            Some("con_marketing_read_1974".to_owned()),
            Some(max_charge),
        )?;
        assert_eq!(
            flags,
            (Some("con_marketing_read_1974".to_owned()), Some(max_charge)),
            "the connection and minor-unit ceiling must pass through unchanged"
        );
    }
    Ok(())
}

#[test]
fn cad1974_refusal_plain_tool_without_flags_remains_admitted() -> Result<()> {
    let flags = connected_read_flags(
        "plain_tool",
        ToolFacts {
            direct: true,
            connected_read: false,
        },
        None,
        None,
    )?;
    assert_eq!(flags, (None, None));
    Ok(())
}
