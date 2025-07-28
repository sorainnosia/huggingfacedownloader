use reqwest::{Client};
use clap::Parser;
use serde::Deserialize;
use serde_json;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use sha2::{Sha256, Digest};
use tokio::fs;
use std::collections::HashMap;
use crate::smart_dl;
use crate::taskwait;
use crate::async_taskwait::AsyncTaskWait;
use crate::config::{AppContext, AppConfig};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::sync::{Semaphore, OwnedSemaphorePermit};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct RepoFile {
    pub rfilename: String,
}

pub async fn fetch_huggingface_repo_files(context: Arc<AppContext>, client: &Client, repo: &str, mut multi: Arc<MultiProgress>) -> Result<Vec<RepoFile>, Box<dyn std::error::Error + Send + Sync>> {
	let mut repo_type = "models".to_string();
	if let Some(c) = &context.config {
		repo_type = c.repo_type.to_string();
	}
	
    let url = format!("https://huggingface.co/api/{}/{}", repo_type, repo);
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .header("User-Agent", "huggingface-downloader")
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let files = serde_json::from_value(resp["siblings"].clone())?;
    Ok(files)
}

pub async fn fetch_sha256_hash(client: &Client, url: &str) -> Option<String> {
    let hash_url = format!("{}.sha256", url);

    match client.get(&hash_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(text) = resp.text().await {
                Some(text.split_whitespace().next()?.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

pub async fn get_sha256_from_etag(client: &Client, url: &str) -> Option<String> {
    if let Ok(resp) = client.head(url).send().await {
        if let Some(etag) = resp.headers().get("x-linked-etag")
            .or_else(|| resp.headers().get("etag")) 
        {
            let tag = etag.to_str().ok()?;
            Some(tag.trim_matches('"').to_string())
        } else {
            None
        }
    } else {
        None
    }
}

pub async fn compute_sha256<P: AsRef<Path>>(path: P) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let file = File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

pub async fn download_repo_files(
	context: Arc<AppContext>,
	repo_type: String,
    client: reqwest::Client,
    repo: &str,
    files: Vec<RepoFile>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target_dir = repo.replace("/", "_");
	
	let mut max_parallel = 4;
	let mut max_chunk = 4;
	if let Some(conf) = &context.config {
		max_parallel = conf.max_parallel;
		max_chunk = conf.max_chunk;
	}
	
	//println!("Max Parallel {}", max_parallel);
	//println!("Max Chunk {}", max_chunk);
	let mut invalid_files: Vec<String> = vec![];
	let mut sha_map: HashMap<String, String> = HashMap::new();
	
	let cancel_notify = Arc::new(tokio::sync::Notify::new());
    for f in files {
        let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
		if repo_type.to_string() == "datasets".to_string() {
			remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
		}
		if repo_type.to_string() == "spaces".to_string() {
			remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
		}
		let local_path = format!("{}/{}", target_dir, f.rfilename);
		
		//if let Some(hash) = fetch_sha256_hash(&client, &remote_url).await {
			//sha_map.insert(remote_url.clone(), hash);
		//}
		
		if tokio::fs::try_exists(&local_path).await? {
			println!("✅ Skipped (exists): {}", &local_path);
			continue;
			let server_hash = sha_map.get(&remote_url);
			let client_hash = compute_sha256(Path::new(&local_path)).await;
			if let Some(s) = server_hash {
				if let Ok(c) = client_hash {
					if s.to_string() == c.to_string() {
						continue;
					} else {
						//fs::remove_file(&local_path).await;
						//invalid_files.push(local_path.to_string());
						//println!("Hash unmatched, file deleted: {}", local_path);
					}
				} else {
					//fs::remove_file(&local_path).await;
					//invalid_files.push(local_path.to_string());
					//println!("Client Hash unmatched, file deleted: {}", local_path);
				}
			} else {
				//fs::remove_file(&local_path).await;
				//invalid_files.push(local_path.to_string());
				//println!("Server Hash unmatched, file deleted: {}", local_path);
			}
		}
	
        if let Some(parent) = std::path::Path::new(&local_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
		
		{
			let run = *taskwait::ISRUNNING.lock().unwrap();
			if run == false {
				return Ok(());
			}
		}
		
		{
			let context2 = context.clone();
			let client2 = client.clone();
			let remote_url2 = remote_url.clone();
			let local_path2 = local_path.clone();
			let multi2 = multi.clone();
			
			let ct = context.taskwait.clone();
			let tw = &mut *ct.lock().unwrap();
			
			if let Some(c) = tw {
				let permit: OwnedSemaphorePermit = c.acquire_owned().await?;

				let handle = smart_dl::smart_download(Some(permit), context2.clone(), &client2, &remote_url2, &local_path2, max_parallel as usize, max_chunk as usize, cancel_notify.clone(), multi.clone()).await;
				taskwait::add_task_handle(handle);
			} else {
				taskwait::wait_available_thread(max_parallel as i32);

				let handle = smart_dl::smart_download(None, context2.clone(), &client2, &remote_url2, &local_path2, max_parallel as usize, max_chunk as usize, cancel_notify.clone(), multi.clone()).await;
				taskwait::add_task_handle(handle);
			}
			
			if tokio::fs::try_exists(&local_path2).await? {
				let server_hash = sha_map.get(&remote_url2);
				let client_hash = compute_sha256(Path::new(&local_path2)).await;
				if let Some(s) = server_hash {
					if let Ok(c) = client_hash {
						if s.to_string() == c.to_string() {
							continue;
						} else {
							//fs::remove_file(&local_path2).await;
							//invalid_files.push(local_path2.to_string());
							//println!("Hash unmatched, file deleted: {}", local_path2);
						}
					}
				}
			}
		}
    }
	
	if invalid_files.len() > 0 {
		println!("File not downloaded because of problems (Hash):");
		for file in invalid_files {
			println!("  {}", file);
		}
	}
    Ok(())
}
