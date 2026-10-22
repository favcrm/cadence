//! One integration-test binary for the `tests/*.rs` checks that do not
//! re-exec themselves.
//!
//! CAD-1727: `autotests = false` in `Cargo.toml` stops Cargo turning each
//! `tests/*.rs` file into its own linked executable. The 84 files below are
//! modules of this target; fifteen files stay separate declared targets:
//! fourteen spawn `current_exe()` with `--exact <fn>` (their child filter
//! only matches an unprefixed test name) and cad1425 is built alone by the
//! cross-build legs. None of those test files is edited (they are acceptance
//! checks), and nextest still runs every test in its own process, so
//! isolation and the suite-lock rules are unchanged.
//!
//! A file that is gated by `#![cfg(feature = "test-seam")]` keeps that gate
//! as a module attribute, so the suite still builds without the feature.
//!
//! `autotests = false` means a new `tests/*.rs` file is NOT built unless it
//! is added here as a `mod` (or declared as its own `[[test]]` in
//! `Cargo.toml`). CI's target inventory comes from `cargo metadata`, so a
//! forgotten registration is a silent coverage hole: add the `mod` in the
//! same change that adds the file.

// Five acceptance files each include `fixtures/aos_media_door.rs` with their
// own `#[path] mod` so each stays self-contained; before CAD-1727 they were
// separate crates, where that is not a duplicate. The inclusion is
// module-scoped and intentional.
#![allow(clippy::duplicate_mod)]

mod cad1041_send_now;
mod cad1119_install_consent;
mod cad1123_hp4_acceptance;
mod cad1126_hosted_smtp;
mod cad1129_explorer_http;
mod cad1140_context_scope_carry;
mod cad1140_upgrade_binding_carry;
mod cad1142_actor_fatal_privacy_acceptance;
mod cad1142_quote_authority_acceptance;
mod cad1142_reason_privacy_acceptance;
mod cad1142_transient_quote;
mod cad1142_unknown_fence_privacy_acceptance;
mod cad1154_compat_semver;
mod cad1184_cli_confinement;
mod cad1186_install_pin;
mod cad1187_dev_mode_acceptance;
mod cad1193_metadata_transport;
mod cad1193_operator_metadata;
mod cad1197_test_env_scrub_acceptance;
mod cad1204_readoption_report;
mod cad1206_board_identity_refusal;
mod cad1207_board_start_acceptance;
mod cad1209_install_vet;
mod cad1235_update_no_downgrade;
mod cad1239_archive_daemon_acceptance;
mod cad1251_acceptance;
mod cad1251_board_start_budget;
mod cad1267_hosted_publish_acceptance;
mod cad1268_aggregate_budget;
mod cad1290_acceptance;
mod cad1290_use_destination;
mod cad1291_acceptance;
mod cad1301_destinations_shape;
mod cad1301_hosted_destinations_acceptance;
mod cad1304_acceptance;
mod cad1304_instagram_image_publish;
mod cad1315_acceptance;
mod cad1318_fresh_registry;
mod cad1328_acceptance;
mod cad1331_locale_member_refusal;
mod cad1332_acceptance;
mod cad1349_cargo_shim_acceptance;
mod cad1353_reviewer_a6_acceptance;
mod cad1374_chat_identity_acceptance;
mod cad1374_explorer_attribution_acceptance;
mod cad1374_issue_wiki_activity_acceptance;
mod cad1374_membership_proof_acceptance;
mod cad1401_wiki_boundary_acceptance;
mod cad1408_audit_prefix_acceptance;
mod cad1433_project_creation_acceptance;
mod cad1449_pi_provider_dependencies;
mod cad1452_auth_boundary;
mod cad1453_member_search_acceptance;
mod cad1455_retained_supported_gh_qa;
mod cad1455_transport_bound_qa;
mod cad1456_bounded_read_refusal_acceptance;
mod cad1467_wiki_blob_read_guard_acceptance;
mod cad1472_guidance;
mod cad1479_client_release_source_acceptance;
mod cad1543_smtp_custody_refusal_acceptance;
mod cad1545_runtime_ownership_acceptance;
mod cad1548_agents_settings_refusal_acceptance;
mod cad1586_settings_management_acceptance;
mod cad1591_peer_provenance_acceptance;
mod cad1614_operator_attribution_acceptance;
mod cad1624_omp_gate_refusal_acceptance;
mod cad1638_guest_directory_refusal;
mod cad1703_my_work_acceptance;
mod cad1713_dev_provider_gate;
mod cad508_restart_window;
mod cad832_sharing_refusal;
mod cad848_lifecycle_serialization;
mod cad848_retention_refusal;
mod cad848_review_lifecycle;
mod cad864_app_views;
mod cad913_wiki_envelope_upload;
mod company_skill_install;
mod pi_master_reset;
mod reference_pack_templates;
mod safety_floor;
mod ui_pidfile;
mod web_client_only_routing_acceptance;
mod web_publish_refusal_acceptance;
mod wiki_board_http_guard_acceptance;
