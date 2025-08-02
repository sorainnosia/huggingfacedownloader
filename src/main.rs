#[macro_use]
extern crate lazy_static;

use reqwest::{Client, header::HeaderMap, header::RANGE, header::AUTHORIZATION, header::HeaderValue};
use scraper::{Html, Selector};
use tokio::{
    fs::{File, remove_file},
    io::AsyncWriteExt,
    sync::{mpsc, Semaphore, Notify},
    signal,
};
use futures_util::StreamExt;
use futures::future::join_all;
use std::sync::{Arc, Mutex};
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::{time::Duration};
use uuid;
use clap::{Parser, ArgAction};
use serde::Deserialize;

mod config;
mod smart_dl;
mod async_taskwait;
mod taskwait;
mod hf;
use hf::*;
use std::num::ParseFloatError;
use crate::smart_dl::*;
use crate::config::*;
use crate::async_taskwait::AsyncTaskWait;

#[derive(Parser)]
pub struct Args {
    #[arg(short = 'j', long, help = "HuggingFace username/repository")]
    pub repo: String,
	
	#[arg(short = 'm', long, default_value = "1", help = "Max parallel file downloads")]
    pub max_parallel: u32,
	
	#[arg(short = 'c', long, default_value = "7", help = "Max chunk per file download")]
    pub max_chunk: u32,
	
	#[arg(short = 'r', long, default_value = "10", help = "Max number of retries")]
    pub max_try: u32,
	
	#[arg(short = 't', long, default_value = "models", help = "Repository Type : models, datasets or spaces")]
    pub repo_type: String,
	
	#[arg(short = 'k', long, default_value = "", help = "HuggingFace user access token (private repository)")]
    pub token: String, 
	
	#[arg(short = 'p', long, help = "Download file by size slices instead of fix chunk")]
	pub max_size: Option<String>,
	
	#[arg(short = 'n', long, action = ArgAction::SetFalse, default_value_t = true, help = "Turn on to make download non resumable")]
    pub resumable: bool,
}

pub fn parse_size_to_bytes(input: &str) -> Result<u64, String> {
    let input = input.trim().to_uppercase();

    let units = ["PB", "TB", "GB", "MB", "KB", "B"];
    let mut number_part = input.clone();
    let mut unit_part = "B";

    for unit in &units {
        if let Some(idx) = input.find(unit) {
            number_part = input[..idx].trim().to_string();
            unit_part = unit;
            break;
        }
    }

    if number_part == input {
        number_part = input.clone();
        unit_part = "B";
    }

    let value: f64 = number_part
        .parse()
        .map_err(|e: ParseFloatError| format!("Failed to parse number: {}", e))?;

    let multiplier: u64 = match unit_part.to_uppercase().as_str() {
        "B" => 1,
        "KB" => 1024,
        "MB" => 1024_u64.pow(2),
        "GB" => 1024_u64.pow(3),
        "TB" => 1024_u64.pow(4),
        "PB" => 1024_u64.pow(5),
        _ => return Err(format!("Unknown unit '{}'", unit_part)),
    };

    Ok((value * multiplier as f64).round() as u64)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

	if args.repo_type.to_lowercase() != "models".to_string() && args.repo_type.to_lowercase() != "datasets" &&
		args.repo_type.to_lowercase() !="spaces" {
		println!("Repository type must be either models/datasets/spaces");
		return Ok(());
	}

	let mut ctx = AppContext::new();
	let mut repo_type = "models".to_string();
	ctx.run();
    config::set_config(&mut ctx);
	
	let mut bytes = 0;
	if let Some(b) = args.max_size.clone() {
		if let Ok(size) = parse_size_to_bytes(&b) {
			bytes = size;
		}
	} else if args.max_size.is_none() == false {
		println!("Slice size must be value, eg 100MB");
		return Ok(());
	}
	
	let mut resumable = false;
	let mut maxtry = 10;
	if let Some(mut c) = ctx.config {
		c.max_parallel = args.max_parallel;
		c.repo_type = args.repo_type;
		c.resumable = args.resumable;
		c.max_chunk = args.max_chunk;
		if bytes == 0 {
			c.max_size = None;
		} else {
			c.max_size = Some(bytes);
		}
		maxtry = args.max_try;
		resumable = c.resumable;
		repo_type = c.repo_type.to_string();
		ctx.taskwait = Arc::new(Mutex::new(Some(AsyncTaskWait::new(c.max_parallel as i32))));
		ctx.config = Some(c);
	}

	*taskwait::ISRUNNING.lock().unwrap() = true;

	let mut context = Arc::new(ctx);
    let repo = args.repo;
	let m = Arc::new(MultiProgress::new());
    
    let mut client = reqwest::Client::new();
	if args.token.is_empty() == false {
		client = Client::builder()
		.default_headers({
			let mut headers = HeaderMap::new();
			headers.insert(
				AUTHORIZATION,
				HeaderValue::from_str(&format!("Bearer {}", args.token)).unwrap(),
			);
			headers
		})
		.build()?;
	}
	
	let files = hf::fetch_huggingface_repo_files(context.clone(), &client, &repo, m.clone()).await?;
    hf::download_repo_files(context.clone(), repo_type.to_string(), client, maxtry, &repo, files, m.clone(), resumable).await?;

	taskwait::wait_all_tasks().await;
    Ok(())
}