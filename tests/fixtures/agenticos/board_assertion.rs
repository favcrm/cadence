use crate::board_identity::{parse, verify, Config, PublicKey, Role};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

// CAD-2153: the platform's real minted sample. The fixture is a
// byte-identical copy of agenticos-v2's
// `apps/api/src/board/fixtures/board-assertion.contract.json`, made with
// `mintBoardAssertion`, a synthetic public-vector key and a fixed clock.
// A drift between the two copies is caught only when one side updates;
// refresh by regenerating in agenticos-v2 and copying the file unchanged.
const MINTED: &str = include_str!("board-assertion.contract.json");

fn minted() -> (String, Config, PublicKey, i64) {
    let f: Value = serde_json::from_str(MINTED).unwrap();
    let text = |k: &str| f[k].as_str().unwrap().to_string();
    let mut x = [0u8; 32];
    x.copy_from_slice(&URL_SAFE_NO_PAD.decode(text("publicKeyX")).unwrap());
    let config = Config {
        host: text("host"),
        issuer: text("issuer"),
        company: text("company"),
    };
    let key = PublicKey {
        kid: text("kid"),
        x,
    };
    (text("assertion"), config, key, f["now"].as_i64().unwrap())
}

pub(crate) fn check() {
    let (token, config, key, now) = minted();
    let id = verify(&parse(&token).unwrap(), &config, &key, now).unwrap();
    assert_eq!(id.role, Role::Operator);
    assert_eq!(id.sub, "usr_synthetic");

    // Same signature, claims edited to a different role: refused.
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    let mut claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
    claims["role"] = json!("member");
    parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let forged = parse(&parts.join(".")).unwrap();
    assert_eq!(
        verify(&forged, &config, &key, now).unwrap_err().code,
        "assertion_invalid"
    );

    // One flipped signature byte: refused.
    let mut sig = URL_SAFE_NO_PAD
        .decode(token.rsplit('.').next().unwrap())
        .unwrap();
    sig[0] ^= 1;
    let head = token.rsplit_once('.').unwrap().0;
    let bad_sig = parse(&format!("{head}.{}", URL_SAFE_NO_PAD.encode(sig))).unwrap();
    assert_eq!(
        verify(&bad_sig, &config, &key, now).unwrap_err().code,
        "assertion_invalid"
    );
}
