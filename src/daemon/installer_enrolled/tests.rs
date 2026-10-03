//! Dependency-explicit public-synthetic crypto/owner mechanics plus ordinary
//! FD/socket outcomes. NO privileged drop/live observer/production constructor.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD,Engine};
use ring::signature::{Ed25519KeyPair,KeyPair};
use serde_json::{Value,json};
use std::cell::Cell;
fn fixture()->Value {serde_json::from_str(include_str!("../fixtures/installer-enrollment-wire-v1.json")).unwrap()}
fn hex(s:&str)->Vec<u8> {(0..s.len()).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).unwrap()).collect()}
fn keys()->Vec<receipt::TrustedKey> {
    let v=fixture();let v=&v["vectors"][0];
    vec![receipt::TrustedKey::capture_test("agenticos-native-owner","synthetic-owner-0001",1,hex(v["publicKey"].as_str().unwrap()).try_into().unwrap())]
}
fn public_fixture_key()->Ed25519KeyPair {
    // Existing published RFC8032 vector seed, no operational key generation.
    Ed25519KeyPair::from_seed_unchecked(&hex(fixture()["seedHex"].as_str().unwrap())).unwrap()
}
fn grant_keys()->Vec<Vec<u8>> {
    vec![format!("synthetic-grant:{}",URL_SAFE_NO_PAD.encode(public_fixture_key().public_key().as_ref())).into_bytes()]
}
fn signed_grant(challenge:Value)->String {
    // Test-only existing grant domain, not an operational signer/authority.
    let h=URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"synthetic-grant"}"#);
    let p=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"challenge":challenge,"nbf":1700000000u64,"exp":1700000060u64})).unwrap());
    let msg=format!("cadence.supervisor-launch-grant.v1\0{h}.{p}");
    let s=URL_SAFE_NO_PAD.encode(public_fixture_key().sign(msg.as_bytes()).as_ref());format!("{h}.{p}.{s}")
}
fn frame(index:usize)->Vec<u8> {
    let v=fixture();let v=&v["vectors"][index];
    format!("enrolled-install-r3 {} {}\n",signed_grant(v["payload"]["binding"]["challenge"].clone()),v["envelope"].as_str().unwrap()).into_bytes()
}
fn synthetic_ms()->Result<u64> {Ok(1700000000001)}
fn clock()->RequestClock {RequestClock {until:Instant::now()+BUDGET,last_ms:1700000000000,now:synthetic_ms}}
fn verified()->VerifiedInstaller {
    let bytes=frame(0);let k=keys();let g=grant_keys();let refs:Vec<_>=g.iter().map(Vec::as_slice).collect();
    VerifiedInstaller::verify(&Frame::parse(&bytes).unwrap(),&k,&refs,&mut clock()).unwrap()
}
struct SyntheticOwner {
    binding:Vec<u8>,stamp:OwnerStamp,phase:Cell<Phase>,consumes:Cell<u32>,observes:Cell<u32>,
    lose_consume:bool,change_before:bool,change_after:bool,
}
impl SyntheticOwner {
    fn new(v:&VerifiedInstaller)->Self {Self {binding:v.receipt.binding_json().to_vec(),stamp:OwnerStamp {
        global:b"synthetic-global-owner-1".to_vec(),company:b"synthetic-company-owner-1".to_vec(),epoch:v.grant.claims().challenge.launch.epoch,
        lineage:v.grant.claims().challenge.lineage.clone(),closure:Closure::Open},phase:Cell::new(Phase::Prepared),consumes:Cell::new(0),observes:Cell::new(0),lose_consume:false,change_before:false,change_after:false}}
    fn snapshot(&self)->OwnerSnapshot {let mut stamp=self.stamp.clone();
        if (self.change_before&&self.observes.get()>1)||(self.change_after&&self.consumes.get()>0) {stamp.company=b"synthetic-replaced-owner".to_vec();}
        OwnerSnapshot {binding_json:self.binding.clone(),phase:self.phase.get(),stamp}}
}
impl CombinedConsume for SyntheticOwner {
    fn observe_prepared(&self,_:&VerifiedInstaller,_:Instant)->Result<OwnerSnapshot> {self.observes.set(self.observes.get()+1);Ok(self.snapshot())}
    fn consume_once(&self,_:&PreparedInstaller<'_>,_:Instant)->Result<grant::ConsumeOutcome> {
        self.consumes.set(self.consumes.get()+1);
        if self.phase.get()!=Phase::Prepared {return Ok(grant::ConsumeOutcome::Unknown);}
        self.phase.set(Phase::Consumed); // even lost ACK retains resolved mechanics state
        Ok(if self.lose_consume {grant::ConsumeOutcome::Unknown}else{grant::ConsumeOutcome::Consumed})
    }
    fn observe_consumed_current(&self,_:&PreparedInstaller<'_>,_:Instant)->Result<OwnerSnapshot> {Ok(self.snapshot())}
}

#[test]
fn actual_signed_formats_require_whole_challenge_and_optional_presence() {
    let v=fixture();let k=keys();let g=grant_keys();let refs:Vec<_>=g.iter().map(Vec::as_slice).collect();
    for i in [0,1,2] {let bytes=frame(i);VerifiedInstaller::verify(&Frame::parse(&bytes).unwrap(),&k,&refs,&mut clock()).unwrap();}
    let baseline=v["vectors"][0]["payload"]["binding"]["challenge"].clone();
    // Signed grant with omitted imageLane != signed receipt with explicit baseline.
    let bytes=format!("enrolled-install-r3 {} {}\n",signed_grant(baseline.clone()),v["vectors"][1]["envelope"].as_str().unwrap());
    assert!(VerifiedInstaller::verify(&Frame::parse(bytes.as_bytes()).unwrap(),&k,&refs,&mut clock()).is_err());
    for pointer in ["/launch/epoch","/launch/request/challenge","/launch/request/identity/company","/launch/request/identity/instance",
        "/launch/request/identity/tier","/launch/request/identity/generation","/launch/request/purpose","/recipient/pid","/recipient/starttime",
        "/recipient/generation","/recipient/nonce","/pins/source","/pins/helper","/pins/node","/pins/piGraph","/pins/policy","/lineage/reference","/lineage/databaseEpoch"] {
        let mut changed=baseline.clone();let field=changed.pointer_mut(pointer).unwrap();
        *field=match field {Value::Number(n)=>json!(n.as_u64().unwrap()+1),Value::String(s)=>json!(match pointer {
            "/launch/request/identity/tier"=>"standard-1".to_string(),"/launch/request/purpose"=>"reconstruct".to_string(),
            "/recipient/starttime"=>"1235".to_string(),_=>{let mut bytes=s.clone().into_bytes();bytes[0]=if bytes[0]==b'a'{b'b'}else{b'a'};String::from_utf8(bytes).unwrap()}}),_=>panic!("fixture scalar")};
        let bytes=format!("enrolled-install-r3 {} {}\n",signed_grant(changed),v["vectors"][0]["envelope"].as_str().unwrap());
        assert!(VerifiedInstaller::verify(&Frame::parse(bytes.as_bytes()).unwrap(),&k,&refs,&mut clock()).is_err(),"{pointer}");
    }
    // Signature-domain cross-use, not syntax alone.
    let e=v["vectors"][0]["envelope"].as_str().unwrap();let swapped=format!("enrolled-install-r3 {e} {e}\n");
    assert!(VerifiedInstaller::verify(&Frame::parse(swapped.as_bytes()).unwrap(),&k,&refs,&mut clock()).is_err());
}

#[test]
fn exact_prepared_record_mutations_and_phase_closure_epoch_lineage_refuse_without_consume() {
    let v=verified();
    for pointer in ["/version","/installer/pid","/installer/starttime","/installer/uid","/installer/gid","/installer/clientDigest","/installer/carrierDigest","/installer/observerDigest",
        "/barrierNonce","/expiresAtMs","/challenge/recipient/pid","/challenge/recipient/starttime","/challenge/recipient/generation","/challenge/recipient/nonce",
        "/challenge/launch/epoch","/challenge/launch/request/identity/generation","/challenge/lineage/reference","/challenge/pins/helper"] {
        let mut record:Value=serde_json::from_slice(v.receipt.binding_json()).unwrap();
        let field=record.pointer_mut(pointer).unwrap();*field=match field {Value::Number(n)=>json!(n.as_u64().unwrap()+1),Value::String(s)=>json!(format!("{s}x")),_=>panic!("fixture scalar")};
        let mut owner=SyntheticOwner::new(&v);owner.binding=serde_json::to_vec(&record).unwrap();
        assert!(prepared(&v,&owner,&mut clock()).is_err(),"{pointer}");assert_eq!(owner.consumes.get(),0);
    }
    for variant in 0..7 {
        let mut owner=SyntheticOwner::new(&v);
        match variant {0=>owner.phase.set(Phase::Consumed),1=>owner.stamp.closure=Closure::Closed,2=>owner.stamp.closure=Closure::Unknown,
            3=>owner.stamp.epoch+=1,4=>owner.stamp.lineage.database_epoch+=1,5=>owner.stamp.company.clear(),_=>owner.stamp.global=vec![0;257]}
        assert!(prepared(&v,&owner,&mut clock()).is_err());assert_eq!(owner.consumes.get(),0);
    }
    let mut owner=SyntheticOwner::new(&v);owner.binding.push(b' ');assert!(prepared(&v,&owner,&mut clock()).is_err());
}
#[test]
fn consume_once_owner_replacement_lost_ack_peer_failure_and_replay_mechanics() {
    let v=verified();let owner=SyntheticOwner::new(&v);let mut c=clock();
    let p=prepared(&v,&owner,&mut c).unwrap();consume_prepared(p,&owner,&mut c,||Ok(())).unwrap();
    assert_eq!(owner.consumes.get(),1);assert_eq!(owner.phase.get(),Phase::Consumed);
    assert!(prepared(&v,&owner,&mut c).is_err());assert_eq!(owner.consumes.get(),1);
    for variant in 0..4 {
        let mut owner=SyntheticOwner::new(&v);owner.lose_consume=variant==0;owner.change_before=variant==1;owner.change_after=variant==2;
        let mut c=clock();let p=prepared(&v,&owner,&mut c).unwrap();
        let error=consume_prepared(p,&owner,&mut c,||if variant==3 {Err(unknown())}else{Ok(())}).err().unwrap();
        assert!(matches!(error,Error::OutcomeUnknown(_)));
        assert_eq!(owner.consumes.get(),if variant==0||variant==2 {1}else{0});
        if owner.consumes.get()==1 {assert_eq!(owner.phase.get(),Phase::Consumed);assert!(prepared(&v,&owner,&mut c).is_err());}
    }
    let owner=SyntheticOwner::new(&v);
    let mut expired=clock();expired.now=||Ok(1700000001000);assert!(prepared(&v,&owner,&mut expired).is_err());
    let mut rollback=clock();rollback.now=||Ok(1699999999999);assert!(prepared(&v,&owner,&mut rollback).is_err());
    let mut late=clock();late.until=Instant::now()-Duration::from_millis(1);assert!(prepared(&v,&owner,&mut late).is_err());assert_eq!(owner.consumes.get(),0);
}
#[test]
fn acknowledgement_distinguishes_signed_attempt_and_recipient_and_never_launch() {
    let v=verified();let b=v.binding();let ack=acknowledgement(b);
    assert_eq!(std::str::from_utf8(&ack).unwrap(),"ok enrolled-consumed-r3 11111111-1111-4111-8111-111111111111 1 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 33333333-3333-4333-8333-333333333333\n");
    classify_ack(b,&ack).unwrap();
    for bad in [b"ok consumed-v1\n".to_vec(),ack[..ack.len()-1].to_vec(),[ack.clone(),b"extra\n".to_vec()].concat(),
        std::str::from_utf8(&ack).unwrap().replace(" 1 "," 2 ").into_bytes(),std::str::from_utf8(&ack).unwrap().replace(" bbbb"," abbb").into_bytes(),
        std::str::from_utf8(&ack).unwrap().replace("33333333-3333","43333333-3333").into_bytes()] {
        assert!(matches!(classify_ack(b,&bad),Err(Error::OutcomeUnknown(_))));
    }
}
#[test]
fn real_named_socket_carries_both_envelopes_with_eof_and_no_caller_authority() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("r3.sock");
    let listener=std::os::unix::net::UnixListener::bind(&path).unwrap();let bytes=frame(0);let expected=bytes.clone();
    let worker=std::thread::spawn(move|| {
        let (stream,_)=listener.accept().unwrap();let until=Instant::now()+BUDGET;
        let received=SecretFrame(transport::read_response(&stream,transport::Deadline::until(until)).unwrap());
        assert_eq!(received.0,expected);let parsed=Frame::parse(&received.0).unwrap();
        assert_ne!(parsed.grant,parsed.receipt);
        // Actual kernel credentials on this ordinary UID socket, not a mock.
        assert_ne!(unsafe {libc::getuid()},21000,"ordinary fixture UID");assert!(socket_peer(&stream).is_err());
        let before=effect_counts();assert!(handle_enrolled_stream(&stream).is_err());assert_eq!(effect_counts(),before);
    });
    let stream=UnixStream::connect(path).unwrap();transport::write_frame(&stream,&bytes,transport::Deadline::until(Instant::now()+BUDGET)).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();worker.join().unwrap();
    let prefix=b"enrolled-install-r3 a.a.a ";let tail=b".a.a\n";let mut max=prefix.to_vec();
    max.extend(vec![b'a';MAX_FRAME-prefix.len()-tail.len()]);max.extend(tail);assert_eq!(max.len(),MAX_FRAME);Frame::parse(&max).unwrap();
    max.insert(prefix.len(),b'a');assert!(Frame::parse(&max).is_err());
    for bytes in [b"".as_slice(),b"enrolled-install-r3 a.a.a b.b.b",b"enrolled-install-r3 a.a.a b.b.b\nextra\n",b"enrolled-install-r3  a.a.a b.b.b\n"] {assert!(Frame::parse(bytes).is_err());}
}

// Coordinator-owned new authority guard is appended unchanged below when supplied.
