// SUSI AI Evaluation Metrics
// Compares local model accuracy against cloud models (like Gemini) on standard benchmarks
// Provides a fast, built-in MMLU-style and logic evaluation runner.

use std::path::Path;

pub struct EvalQuestion {
    pub category: &'static str,
    pub prompt: &'static str,
    pub expected_substrings: &'static [&'static str],
}

pub const EVAL_DATASET: &[EvalQuestion] = &[
    EvalQuestion {
        category: "Mathematics",
        prompt: "If you have a square with a side length of 4, what is its area?\nA) 8\nB) 12\nC) 16\nD) 20\n\nProvide only the letter of the correct answer.",
        expected_substrings: &["C", "16"],
    },
    EvalQuestion {
        category: "Logical Reasoning",
        prompt: "All glarbs are snorfs. Some snorfs are blings. Therefore, all glarbs are blings. Is this statement True or False?\nProvide only 'True' or 'False'.",
        expected_substrings: &["False", "false"],
    },
    EvalQuestion {
        category: "Coding",
        prompt: "In Python, which keyword is used to define a function?\nA) func\nB) def\nC) define\nD) function\n\nProvide only the letter of the correct answer.",
        expected_substrings: &["B", "def"],
    },
    EvalQuestion {
        category: "Physics",
        prompt: "What is the primary force that keeps planets in orbit around the sun?\nA) Electromagnetism\nB) Strong Nuclear Force\nC) Gravity\nD) Weak Nuclear Force\n\nProvide only the letter of the correct answer.",
        expected_substrings: &["C", "Gravity"],
    }
];

pub struct EvalRunner;

impl EvalRunner {
    fn check_correctness(response: &str, expected: &[&str]) -> bool {
        let upper_resp = response.to_uppercase();
        expected
            .iter()
            .any(|&e| upper_resp.contains(&e.to_uppercase()))
    }

    pub fn run_evaluations(workspace: &Path) -> String {
        let mut out = String::from("# SUSI AI Evaluation Metrics (Local vs Cloud)\n\n");
        out.push_str("Running Built-in MMLU-lite & Logic tests...\n\n");

        let mut local_score = 0;
        let mut cloud_score = 0;
        let mut cloud_attempted = 0;
        let total = EVAL_DATASET.len();

        let api_base = std::env::var("SUSI_BENCH_CLOUD_API_BASE").unwrap_or_default();
        let api_key = std::env::var("SUSI_BENCH_CLOUD_API_KEY").unwrap_or_default();
        let model =
            std::env::var("SUSI_BENCH_CLOUD_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

        let has_cloud = !api_base.trim().is_empty();

        for (i, q) in EVAL_DATASET.iter().enumerate() {
            out.push_str(&format!(
                "## Q{}: [{}] {}\n",
                i + 1,
                q.category,
                q.prompt.replace("\n", " ")
            ));

            // Local Inference
            let local_resp =
                crate::engine::GemiEngine::generate_reasoning_deep(q.prompt, workspace);
            let local_correct = Self::check_correctness(&local_resp, q.expected_substrings);
            if local_correct {
                local_score += 1;
            }

            // Cloud Inference
            let mut cloud_resp = String::from("SKIPPED");
            let mut cloud_correct = false;

            if has_cloud {
                let payload = serde_json::json!({
                    "model": model,
                    "messages": [{"role": "user", "content": q.prompt}],
                    "max_tokens": 50
                });

                let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
                let mut req = crate::susi_sandbox::manager::http_agent()
                    .post(&url)
                    .header("Content-Type", "application/json");
                if !api_key.is_empty() {
                    req = req.header("Authorization", format!("Bearer {}", api_key));
                }

                if let Ok(resp) = req.send_json(payload) {
                    let body: serde_json::Value = resp
                        .into_body()
                        .read_json()
                        .unwrap_or(serde_json::json!({}));
                    cloud_resp = body["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    cloud_correct = Self::check_correctness(&cloud_resp, q.expected_substrings);
                    if cloud_correct {
                        cloud_score += 1;
                    }
                    cloud_attempted += 1;
                } else {
                    cloud_resp = "API_ERROR".to_string();
                }
            }

            out.push_str(&format!(
                "- **Local Answer**: {} (Correct: {})\n",
                local_resp.trim().replace("\n", " "),
                local_correct
            ));
            out.push_str(&format!(
                "- **Cloud Answer**: {} (Correct: {})\n\n",
                cloud_resp.trim().replace("\n", " "),
                cloud_correct
            ));
        }

        out.push_str("## Final Accuracy Metrics\n\n");
        out.push_str(&format!(
            "- **Local Model Accuracy:** {:.1}% ({}/{})\n",
            (local_score as f32 / total as f32) * 100.0,
            local_score,
            total
        ));

        if cloud_attempted > 0 {
            out.push_str(&format!(
                "- **Cloud Model Accuracy:** {:.1}% ({}/{})\n",
                (cloud_score as f32 / cloud_attempted as f32) * 100.0,
                cloud_score,
                cloud_attempted
            ));
        } else {
            out.push_str("- **Cloud Model Accuracy:** N/A (Not configured, map SUSI_BENCH_CLOUD_API_BASE to Gemini/OpenAI endpoint)\n");
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_correctness() {
        assert!(EvalRunner::check_correctness(
            "The correct answer is B",
            &["B", "def"]
        ));
        assert!(EvalRunner::check_correctness("define", &["B", "def"]));
        assert!(!EvalRunner::check_correctness(
            "The answer is func",
            &["B", "def"]
        ));
    }
}
