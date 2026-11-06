use ni_proto::{Frame, Mac, MAX_CHUNK};
use proptest::prelude::*;

fn arb_mac() -> impl Strategy<Value = Mac> {
    any::<[u8; 6]>().prop_map(Mac)
}

proptest! {
    #[test]
    fn encode_decode_roundtrip(
        src in arb_mac(),
        dst in arb_mac(),
        seq in any::<u16>(),
        ack in any::<u16>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=MAX_CHUNK),
    ) {
        let frame = Frame::new(src, dst, seq, ack, payload);
        prop_assert_eq!(Frame::decode(&frame.encode()).unwrap(), frame);
    }

    #[test]
    fn decode_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Frame::decode(&buf);
    }
}
