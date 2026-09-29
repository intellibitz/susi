use crate::independent_verify::{verification_satisfied, ReviewConclusion, ToolReceipt};

#[test]
fn vc_201_028_implementer_assertion_insufficient() {
    let c = ReviewConclusion {
        reviewer: "alice".into(),
        pass: true,
        receipts: vec![ToolReceipt {
            tool: "test".into(),
            digest: "d1".into(),
        }],
        implementer: "alice".into(),
    };
    assert!(!verification_satisfied(&c, "alice"));
}

#[test]
fn vc_201_028_requires_receipts_and_independent_reviewer() {
    let c = ReviewConclusion {
        reviewer: "bob".into(),
        pass: true,
        receipts: vec![ToolReceipt {
            tool: "test".into(),
            digest: "d1".into(),
        }],
        implementer: "alice".into(),
    };
    assert!(verification_satisfied(&c, "alice"));
}

#[test]
fn vc_201_028_duplicate_digests_fail() {
    let c = ReviewConclusion {
        reviewer: "bob".into(),
        pass: true,
        receipts: vec![
            ToolReceipt {
                tool: "a".into(),
                digest: "same".into(),
            },
            ToolReceipt {
                tool: "b".into(),
                digest: "same".into(),
            },
        ],
        implementer: "alice".into(),
    };
    assert!(!verification_satisfied(&c, "alice"));
}
