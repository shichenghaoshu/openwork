use openwork_e2e::office_demo::{
    OfficeDemoConfig, OfficePlan, PresentationPlan, SlidePlan, SpreadsheetPlan, run_office_demo,
};
use openwork_execution::{DigestPinnedImageRef, RunStatus};
use std::env;
use std::path::PathBuf;

#[test]
#[ignore = "requires the local OpenWork LibreOffice image and Docker daemon"]
fn real_cli_anything_libreoffice_creates_xlsx_and_pptx() {
    assert_eq!(env::var("OPENWORK_REAL_DOCKER_TESTS").as_deref(), Ok("1"));
    let output = tempfile::tempdir().expect("office demo output root");
    let report = run_office_demo(
        OfficeDemoConfig {
            engine_bin: PathBuf::from(required("OPENWORK_DOCKER_BIN")),
            image: DigestPinnedImageRef::parse(required("OPENWORK_LIBREOFFICE_IMAGE"))
                .expect("digest-pinned LibreOffice image"),
            output_root: output.path().to_path_buf(),
        },
        OfficePlan {
            title: "OpenWork Quarterly Review".to_owned(),
            spreadsheet: SpreadsheetPlan {
                filename: "quarterly-review.xlsx".to_owned(),
                sheet: "Summary".to_owned(),
                rows: vec![
                    vec!["Metric".to_owned(), "Q1".to_owned(), "Q2".to_owned()],
                    vec!["Revenue".to_owned(), "120".to_owned(), "148".to_owned()],
                    vec!["Orders".to_owned(), "42".to_owned(), "51".to_owned()],
                ],
            },
            presentation: PresentationPlan {
                filename: "quarterly-briefing.pptx".to_owned(),
                slides: vec![
                    SlidePlan {
                        title: "Quarterly Review".to_owned(),
                        content: "Revenue and order growth".to_owned(),
                    },
                    SlidePlan {
                        title: "Next steps".to_owned(),
                        content: "Review pipeline and customer follow-up".to_owned(),
                    },
                ],
            },
        },
    )
    .expect("real CLI-Anything LibreOffice demo");
    assert_eq!(report.status, RunStatus::Succeeded);
    assert_eq!(report.artifacts.len(), 2);
    assert!(
        report
            .artifacts
            .iter()
            .all(|artifact| artifact.size_bytes > 0)
    );
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}
