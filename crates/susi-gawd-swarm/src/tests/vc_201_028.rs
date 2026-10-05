use crate::independent_verify::{
    digest_fields, verification_satisfied, ReviewConclusion, ToolReceipt,
};

#[test]
fn vc_201_028_implementer_assertion_insufficient() {
    let c = ReviewConclusion {
        reviewer: "alice".into(),
        pass: true,
        receipts: vec![ToolReceipt {
            tool: "test".into(),
            digest: digest_fields(&["receipt", "d1"]),
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
            digest: digest_fields(&["receipt", "d1"]),
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
                digest: digest_fields(&["receipt", "same"]),
            },
            ToolReceipt {
                tool: "b".into(),
                digest: digest_fields(&["receipt", "same"]),
            },
        ],
        implementer: "alice".into(),
    };
    assert!(!verification_satisfied(&c, "alice"));
}

/// Wiring test: independent verifier evidence is required in the
/// swarm verify path — reviewer must differ from subject, receipts
/// must be non-empty and distinct.
#[test]
fn independent_verify_dispatch_wiring() {
    let subject = "implementer-a";
    let receipts = vec![
        ToolReceipt {
            tool: "tool-1".into(),
            digest: digest_fields(&["receipt", "digest-1"]),
        },
        ToolReceipt {
            tool: "tool-2".into(),
            digest: digest_fields(&["receipt", "digest-2"]),
        },
    ];

    // Same reviewer as subject → not independent.
    let same = ReviewConclusion {
        reviewer: subject.into(),
        pass: true,
        receipts: receipts.clone(),
        implementer: subject.into(),
    };
    assert!(!verification_satisfied(&same, subject));

    // No receipts → fails even with independent reviewer.
    let no_receipts = ReviewConclusion {
        reviewer: "verifier".into(),
        pass: true,
        receipts: vec![],
        implementer: subject.into(),
    };
    assert!(!verification_satisfied(&no_receipts, subject));

    // Correct independent verifier with distinct receipts → satisfied.
    let ok = ReviewConclusion {
        reviewer: "independent-verifier".into(),
        pass: true,
        receipts,
        implementer: subject.into(),
    };
    assert!(verification_satisfied(&ok, subject));
}
