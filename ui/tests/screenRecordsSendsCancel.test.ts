// CAD-1933: the hosted Worker's CRM mount lists `sends.cancel` (AOS #1411).
// parseMount refuses a receipt naming any operation outside RECORD_OPERATIONS,
// so the CRM screen never mounted. The Worker serves it on the call route.
import { parseMount } from "../src/features/workspace-apps/screen/screenLifecycle";
import { RECORD_CALL_VERBS, RECORD_SLOT_VERBS } from "../src/features/workspace-apps/screen/screenProtocol";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const receipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: "a".repeat(64), generation: 1, tag: "crm",
  action_token: "d".repeat(64), tools: [], host_contract: "screen-records.v1",
  operations: ["sends.list", "sends.show", "sends.approve", "sends.cancel"], navigation: null,
  install_id: "install_crm", context_id: "default", digest: "sha256:" + "c".repeat(64),
  display_scope_key: "e".repeat(64), display_session_key: "f".repeat(64), remote_images: [],
};
check(parseMount(receipt, "crm"), "a records mount listing sends.cancel is accepted");
check((RECORD_CALL_VERBS as readonly string[]).includes("sends.cancel"), "sends.cancel is a call verb");
check(!(RECORD_SLOT_VERBS as readonly string[]).includes("sends.cancel"), "sends.cancel is not a slot verb");
check(!parseMount({ ...receipt, operations: [...receipt.operations, "sends.unknown"] }, "crm"), "an unknown operation still refuses the receipt");
console.log("screenRecordsSendsCancel: ok");
