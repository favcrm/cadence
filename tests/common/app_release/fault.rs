//! Trusted in-process provider faults; all publication material still comes
//! from actual writer/reviewer turns and the registered Local implementation.
use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::{AppArtifactError, PlatformAdapter};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) enum Fault {
    UncertainAfterCommit,
    RefusedBeforeCommit,
    Unsupported,
}

struct FaultAdapter {
    inner: Arc<dyn PlatformAdapter>,
    calls: Arc<AtomicUsize>,
    fault: Fault,
}
struct LegacyOnlyAdapter {
    inner: Arc<dyn PlatformAdapter>,
    calls: Arc<AtomicUsize>,
}

// Preserve reviewed descriptor, registration and preparation exactly. The
// fixture cannot manufacture authority, change input, or bypass Local checks.
macro_rules! delegate {
    () => {
        fn table(&self) -> &ToolTable {
            self.inner.table()
        }
        fn connection_descriptor(
            &self,
        ) -> Option<cadence_agent::platform::connections::ProviderDescriptor> {
            self.inner.connection_descriptor()
        }
        fn connection_registration(&self) -> Option<String> {
            self.inner.connection_registration()
        }
        fn prepare_app_text(
            &self,
            title: &str,
            body: &str,
            provenance: &Value,
        ) -> Result<Value, String> {
            self.inner.prepare_app_text(title, body, provenance)
        }
        fn reported_manifest_version(&self) -> Option<String> {
            self.inner.reported_manifest_version()
        }
        fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
            self.inner.preview(account, tool, input)
        }
        fn execute(
            &self,
            credential: &[u8],
            tool: &str,
            input: &Value,
            key: &str,
            hash: Option<&str>,
        ) -> Result<Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(credential, tool, input, key, hash)
        }
        fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
            self.inner.source_hash(agent, source)
        }
        fn implied_source(&self, agent: &str, tool: &str, input: &Value) -> Option<String> {
            self.inner.implied_source(agent, tool, input)
        }
    };
}
impl PlatformAdapter for FaultAdapter {
    delegate!();
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        match self.fault {
            // The trusted fixture knows it refused before any write. Report
            // a simulated verification mismatch to exercise flagged-terminal
            // acknowledgement. This is not Local's absent-item read-back,
            // which honestly reports Unknown.
            Fault::RefusedBeforeCommit => Verified::False,
            _ => self.inner.read_back(tool, input),
        }
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Value, AppArtifactError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.fault {
            Fault::UncertainAfterCommit => {
                self.inner
                    .execute_app_artifact(credential, tool, input, key, hash)?;
                Err(AppArtifactError::Uncertain(
                    "trusted fixture lost confirmation after real Local commit".into(),
                ))
            }
            Fault::RefusedBeforeCommit => Err(AppArtifactError::Refused(
                "trusted fixture refused before commit".into(),
            )),
            Fault::Unsupported => unreachable!("unsupported uses the trait default"),
        }
    }
}
impl PlatformAdapter for LegacyOnlyAdapter {
    delegate!();
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.inner.read_back(tool, input)
    }
    // Deliberately inherit the unsupported typed hook. A legacy execute method
    // and reviewed metadata alone must never permit an app-artifact send.
}

pub(crate) fn wrap(
    opts: &mut cadence_agent::daemon::ServeOptions,
    fault: Fault,
    calls: Arc<AtomicUsize>,
) {
    let inner = opts.platforms.get("local").unwrap().clone();
    let adapter: Arc<dyn PlatformAdapter> = match fault {
        Fault::Unsupported => Arc::new(LegacyOnlyAdapter { inner, calls }),
        _ => Arc::new(FaultAdapter {
            inner,
            calls,
            fault,
        }),
    };
    opts.platforms.insert("local".into(), adapter);
}
