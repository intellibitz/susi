use crate::retrieval_eval::{evaluate_promotion, PromoteVerdict, RetrievalSuite};

#[test]
fn vc_201_082_rejects_correctness_regression_despite_recall() {
    let baseline = RetrievalSuite {
        version: 1,
        answer_correctness: 0.9,
        private_isolation: 1.0,
        recall: 0.5,
        index_size: 100,
    };
    let candidate = RetrievalSuite {
        version: 2,
        answer_correctness: 0.8,
        private_isolation: 1.0,
        recall: 0.99,
        index_size: 10_000,
    };
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::RejectCorrectnessRegression
    );
}

#[test]
fn vc_201_082_rejects_isolation_regression() {
    let baseline = RetrievalSuite {
        version: 1,
        answer_correctness: 0.9,
        private_isolation: 1.0,
        recall: 0.5,
        index_size: 100,
    };
    let candidate = RetrievalSuite {
        version: 2,
        answer_correctness: 0.95,
        private_isolation: 0.5,
        recall: 0.9,
        index_size: 200,
    };
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::RejectIsolationRegression
    );
}

#[test]
fn vc_201_082_promotes_when_quality_holds() {
    let baseline = RetrievalSuite {
        version: 1,
        answer_correctness: 0.9,
        private_isolation: 1.0,
        recall: 0.5,
        index_size: 100,
    };
    let candidate = RetrievalSuite {
        version: 2,
        answer_correctness: 0.91,
        private_isolation: 1.0,
        recall: 0.4,
        index_size: 50,
    };
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::Promote
    );
}

#[test]
fn vc_201_082_promotes_when_correctness_and_isolation_equal_baseline() {
    let baseline = RetrievalSuite {
        version: 1,
        answer_correctness: 0.90,
        private_isolation: 0.88,
        recall: 0.50,
        index_size: 120,
    };
    let equal_cand = RetrievalSuite {
        version: 2,
        answer_correctness: 0.90,
        private_isolation: 0.88,
        recall: 0.65,
        index_size: 135,
    };
    assert_eq!(
        evaluate_promotion(&baseline, &equal_cand),
        PromoteVerdict::Promote
    );
}
