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
use clap::Parser;
use serde::Deserialize;

mod config;
mod smart_dl;
mod async_taskwait;
mod taskwait;
mod hf;
use hf::*;
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
	
	#[arg(short = 't', long, default_value = "models", help = "Repository Type : models, datasets or spaces")]
    pub repo_type: String,
	
	#[arg(short = 'k', long, default_value = "", help = "HuggingFace token")]
    pub token: String,
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
	
	if let Some(mut c) = ctx.config {
		c.max_parallel = args.max_parallel;
		c.max_chunk = args.max_chunk;
		c.repo_type = args.repo_type;
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
    hf::download_repo_files(context.clone(), repo_type.to_string(), client, &repo, files, m.clone()).await?;

	taskwait::wait_all_tasks().await;
    Ok(())
}