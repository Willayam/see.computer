use std::{error::Error, io::Write, path::PathBuf, time::Instant};

use ort::ep::{
    CoreML,
    coreml::{ComputeUnits, ModelFormat},
};
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode};
use serde_json::json;

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32;
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        return Err("usage: see-backend-bench MODE MODEL_DIR RUNS CACHE_DIR WAV...".into());
    }
    unsafe {
        pthread_set_qos_class_self_np(0x19, 0);
    }
    let mode = args[1].clone();
    let runs: usize = args[3].parse()?;
    let cache = PathBuf::from(&args[4]);
    std::fs::create_dir_all(&cache)?;
    let profile = std::env::var_os("SEE_BENCH_PROFILE").is_some();
    let legacy = std::env::var_os("SEE_BENCH_COREML_LEGACY").is_some();
    tracing_subscriber::fmt()
        .with_max_level(if profile {
            tracing_subscriber::filter::LevelFilter::INFO
        } else {
            tracing_subscriber::filter::LevelFilter::WARN
        })
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let threads = if mode == "cpu4" { 4 } else { 8 };
    let backend = mode.clone();
    let config = ExecutionConfig::default()
        .with_intra_threads(threads)
        .with_custom_configure(move |builder| {
            let mut builder = builder.with_prepacking(false)?;
            if backend.starts_with("coreml-") {
                let units = match backend.as_str() {
                    "coreml-gpu" => ComputeUnits::CPUAndGPU,
                    "coreml-ane" => ComputeUnits::CPUAndNeuralEngine,
                    _ => unreachable!(),
                };
                builder = builder.with_execution_providers([CoreML::default()
                    .with_model_format(if legacy {
                        ModelFormat::NeuralNetwork
                    } else {
                        ModelFormat::MLProgram
                    })
                    .with_compute_units(units)
                    .with_model_cache_dir(cache.to_string_lossy())
                    .with_profile_compute_plan(profile)
                    .build()
                    .error_on_failure()])?;
            }
            if profile {
                builder = builder.with_log_level(ort::logging::LogLevel::Verbose)?;
            }
            Ok(builder)
        });
    if !matches!(mode.as_str(), "cpu8" | "cpu4" | "coreml-gpu" | "coreml-ane") {
        return Err("unknown mode".into());
    }
    let start = Instant::now();
    let mut model = ParakeetTDT::from_pretrained(&args[2], Some(config))?;
    println!(
        "{}",
        json!({"event":"load", "mode":mode,"ms":start.elapsed().as_secs_f64()*1000.0})
    );
    let clips = args[5..]
        .iter()
        .map(|path| {
            let mut wav = hound::WavReader::open(path)?;
            let spec = wav.spec();
            assert_eq!(
                (spec.sample_rate, spec.channels, spec.bits_per_sample),
                (16000, 1, 16)
            );
            let samples = wav
                .samples::<i16>()
                .map(|s| s.map(|s| f32::from(s) / 32768.0))
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, hound::Error>((path.clone(), samples))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (path, samples) in &clips {
        let start = Instant::now();
        let result = model.transcribe_samples_with_bias(
            samples.clone(),
            16000,
            1,
            Some(TimestampMode::Sentences),
            None,
        )?;
        println!(
            "{}",
            json!({"event":"warm", "mode":mode, "clip":path, "ms":start.elapsed().as_secs_f64()*1000.0,"text":result.text})
        );
    }
    // The coordinator starts/stops CPU load after model loading and warmup.
    println!("{}", json!({"event":"ready", "mode":mode}));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    for run in 0..runs {
        for (path, samples) in &clips {
            let start = Instant::now();
            let result = model.transcribe_samples_with_bias(
                samples.clone(),
                16000,
                1,
                Some(TimestampMode::Sentences),
                None,
            )?;
            println!(
                "{}",
                json!({"event":"sample", "mode":mode, "clip":path,"run":run,"seconds":samples.len() as f64/16000.0,"ms":start.elapsed().as_secs_f64()*1000.0,"text":result.text})
            );
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}
