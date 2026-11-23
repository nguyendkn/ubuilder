use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use ubuilder::host::Host;
use ubuilder::pipeline::compiler;
use ubuilder::pipeline::parser::ParsedWorkflow;
use ubuilder::pipeline::planner;

fn workflow() -> String {
    let mut jobs = String::new();
    for index in 0..12 {
        jobs.push_str(&format!(
            r#"  job-{index}:
    strategy:
      matrix:
        target: [native, x86_64-unknown-linux-gnu]
        profile: [debug, release]
    steps:
      - name: prepare
        run: echo preparing {index}
      - name: build
        run: echo building {index} ${{{{ matrix.profile }}}}
"#
        ));
    }
    format!("version: 1\nname: Bench\nenv:\n  GLOBAL: value\njobs:\n{jobs}")
}

fn bench(c: &mut Criterion) {
    let source = workflow();
    let root = std::path::Path::new(".");
    let host = Host::detect().expect("host");
    c.bench_function("parse_validate_12_jobs_matrix_4", |b| {
        b.iter(|| {
            let parsed = ParsedWorkflow::parse(black_box(root), black_box(source.clone())).unwrap();
            parsed.validate().unwrap();
        })
    });
    let parsed = ParsedWorkflow::parse(root, source).unwrap();
    parsed.validate().unwrap();
    c.bench_function("compile_12_jobs_matrix_4", |b| {
        b.iter(|| compiler::compile(black_box(&parsed), black_box(&host)).unwrap())
    });
    let ir = compiler::compile(&parsed, &host).unwrap();
    c.bench_function("plan_48_jobs", |b| {
        b.iter(|| planner::plan(black_box(&ir), black_box(root), black_box(&host)).unwrap())
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
