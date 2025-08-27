use reqwest::{Client};
use clap::Parser;
use serde::Deserialize;
use serde_json;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use sha1::Sha1;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lfs: Option<LfsInfo>,
}

#[derive(Debug, Deserialize)]
pub struct LfsInfo {
    pub oid: String,  // SHA256 hash for LFS files
    pub size: i64,
    #[serde(rename = "pointerSize")]
    pub pointer_size: i32,
}

#[derive(Debug)]
pub struct FileVerificationResult {
    pub filename: String,
    pub expected_hash: String,
    pub actual_hash: String,
    pub passed: bool,
}

pub async fn fetch_huggingface_repo_files(context: Arc<AppContext>, client: &Client, repo: &str, mut multi: Arc<MultiProgress>) -> Result<Vec<RepoFile>, Box<dyn std::error::Error + Send + Sync>> {
	let mut repo_type = "models".to_string();
	if let Some(c) = &context.config {
		repo_type = c.repo_type.to_string();
	}
	
    // Use the /tree/main endpoint to get file listings with LFS info
    let url = format!("https://huggingface.co/api/{}/{}/tree/main", repo_type, repo);
    
    //println!("Fetching file list from: {}", url);

    let resp = client
        .get(&url)
        .header("User-Agent", "huggingface-downloader")
        .send()
        .await?;
        
    if !resp.status().is_success() {
        return Err(format!("Failed to fetch repo files: HTTP {}", resp.status()).into());
    }
    
    let json_resp = resp.json::<Vec<serde_json::Value>>().await?;
    
    // Debug: print first file to see structure
    if !json_resp.is_empty() {
        //println!("Sample file structure: {:#?}", &json_resp[0]);
    }
    
    // Convert JSON array to Vec<RepoFile>
    let mut files = Vec::new();
    for item in json_resp {
        // The API returns files with "path" field, but we expect "rfilename"
        if let Some(path) = item.get("path").and_then(|p| p.as_str()) {
            let mut file = RepoFile {
                rfilename: path.to_string(),
                lfs: None,
            };
            
            // Check if there's an lfs field
            if let Some(lfs_data) = item.get("lfs") {
                if let Ok(lfs_info) = serde_json::from_value::<LfsInfo>(lfs_data.clone()) {
                    file.lfs = Some(lfs_info);
                }
            }
            
            files.push(file);
        }
    }
    
    println!("Found {} files, {} with LFS", files.len(), files.iter().filter(|f| f.lfs.is_some()).count());
    
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
            // Remove quotes and sha256: prefix if present
            let cleaned = tag.trim_matches('"');
            if cleaned.starts_with("sha256:") {
                Some(cleaned.trim_start_matches("sha256:").to_string())
            } else {
                Some(cleaned.to_string())
            }
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
    // Use explicit hex encoding like Go's hex.EncodeToString
    let hex_string = result.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    Ok(hex_string)
}

pub async fn compute_sha1<P: AsRef<Path>>(path: P) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let file = File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = reader.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let result = hasher.finalize();
    // Use explicit hex encoding like Go's hex.EncodeToString
    let hex_string = result.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    Ok(hex_string)
}

pub async fn compute_hash_matching_expected<P: AsRef<Path>>(path: P, expected_hash: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Determine hash type based on length
    match expected_hash.len() {
        40 => compute_sha1(path).await,  // SHA-1
        64 => compute_sha256(path).await, // SHA-256
        _ => {
            // Try SHA-256 by default for unknown lengths
            compute_sha256(path).await
        }
    }
}

pub async fn verify_downloaded_files(
    target_dir: &str,
    files: &Vec<RepoFile>,
    sha_map: &HashMap<String, String>,
    repo: &str,
    repo_type: &str,
    client: &Client,
) -> Vec<FileVerificationResult> {
    let mut verification_results = Vec::new();
    
    println!("\n🔍 Verifying downloaded files...");
    
    for f in files {
        let local_path = format!("{}/{}", target_dir, f.rfilename);
        
        // Skip if file doesn't exist
        if !tokio::fs::try_exists(&local_path).await.unwrap_or(false) {
            continue;
        }
        
        // Get expected hash
        let mut expected_hash = None;
        
        // For LFS files, use the oid from metadata
        if let Some(lfs_info) = &f.lfs {
            expected_hash = Some(lfs_info.oid.clone());
        } else {
            // Try to get hash from our sha_map or fetch it
            let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
            if repo_type == "datasets" {
                remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
            } else if repo_type == "spaces" {
                remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
            }
            
            if let Some(hash) = sha_map.get(&remote_url) {
                expected_hash = Some(hash.clone());
            } else {
                // Try fetching from .sha256 file or etag
                if let Some(hash) = fetch_sha256_hash(&client, &remote_url).await {
                    expected_hash = Some(hash);
                } else if let Some(hash) = get_sha256_from_etag(&client, &remote_url).await {
                    expected_hash = Some(hash);
                }
            }
        }
        
        // If we have an expected hash, verify it
        if let Some(expected) = expected_hash {
            print!("Verifying {} ... ", f.rfilename);
            match compute_hash_matching_expected(&local_path, &expected).await {
                Ok(actual_hash) => {
                    let passed = actual_hash == expected;
                    if passed {
                        println!("✅ PASSED");
                    } else {
                        println!("❌ FAILED");
                    }
                    
                    verification_results.push(FileVerificationResult {
                        filename: f.rfilename.clone(),
                        expected_hash: expected,
                        actual_hash,
                        passed,
                    });
                }
                Err(e) => {
                    println!("❌ Error computing hash: {}", e);
                }
            }
        }
    }
    
    verification_results
}

pub async fn download_repo_files(
	context: Arc<AppContext>,
	repo_type: String,
    client: reqwest::Client,
	maxtry: u32,
    repo: &str,
    files: Vec<RepoFile>,
	mut multi: Arc<MultiProgress>,
	resumable: bool, skip_sha: bool
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target_dir = repo.replace("/", "_");
	
	let mut max_parallel = 4;
	let mut max_chunk = 4;
	let mut max_size = None;
	if let Some(conf) = &context.config {
		max_parallel = conf.max_parallel;
		max_chunk = conf.max_chunk;
		max_size = conf.max_size;
        // Add skip_sha field to AppConfig if not present
        // For now, we'll assume it's false
	}
	
	let mut invalid_files: Vec<String> = vec![];
	let mut sha_map: HashMap<String, String> = HashMap::new();
	
	let cancel_notify = Arc::new(tokio::sync::Notify::new());
    
    // Collect SHA256 hashes for all files
    println!("📋 Collecting file metadata...");
    for f in &files {
        let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
        if repo_type == "datasets" {
            remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
        } else if repo_type == "spaces" {
            remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
        }
        
        // For LFS files, store the hash from metadata
        if let Some(lfs_info) = &f.lfs {
            sha_map.insert(remote_url.clone(), lfs_info.oid.clone());
        }
    }
    
    // Download files
    for f in &files {
        let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
		if repo_type.to_string() == "datasets".to_string() {
			remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
		}
		if repo_type.to_string() == "spaces".to_string() {
			remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
		}
		let local_path = format!("{}/{}", target_dir, f.rfilename);
		
		if tokio::fs::try_exists(&local_path).await? {
			// For existing files, only verify LFS files if not skipping SHA
			if !skip_sha && f.lfs.is_some() {
				print!("Verifying existing LFS file {} ... ", &local_path);
				if let Some(lfs_info) = &f.lfs {
					match compute_sha256(&local_path).await {
						Ok(actual_hash) => {
							if actual_hash == lfs_info.oid {
								println!("✅ Hash matched");
								continue;
							} else {
								println!("❌ Hash mismatch, re-downloading");
								// Delete and re-download
								fs::remove_file(&local_path).await?;
							}
						}
						Err(e) => {
							println!("❌ Error computing hash: {}, re-downloading", e);
							fs::remove_file(&local_path).await?;
						}
					}
				}
			} else {
				println!("✅ Skipped (exists): {}", &local_path);
				continue;
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

				let handle = smart_dl::smart_download(Some(permit), context2.clone(), &client2, &remote_url2, &local_path2, maxtry, max_parallel as usize, max_chunk as usize, max_size.clone(), cancel_notify.clone(), multi.clone(), resumable).await;
				taskwait::add_task_handle(handle);
			} else {
				taskwait::wait_available_thread(max_parallel as i32);

				let handle = smart_dl::smart_download(None, context2.clone(), &client2, &remote_url2, &local_path2, maxtry, max_parallel as usize, max_chunk as usize, max_size.clone(), cancel_notify.clone(), multi.clone(), resumable).await;
				taskwait::add_task_handle(handle);
			}
		}
    }
    
    // Wait for all downloads to complete
    taskwait::wait_all_tasks().await;
    
    // Only verify LFS files if skip_sha is false
    if !skip_sha {
        println!("\n🔍 Verifying downloaded LFS files...");
        let mut verification_failed = false;
        let mut failed_files = Vec::new();
        
        for f in &files {
            // Only verify LFS files
            if let Some(lfs_info) = &f.lfs {
                let local_path = format!("{}/{}", target_dir, f.rfilename);
                
                if tokio::fs::try_exists(&local_path).await? {
                    print!("Verifying {} ... ", f.rfilename);
                    
                    match compute_sha256(&local_path).await {
                        Ok(actual_hash) => {
                            if actual_hash == lfs_info.oid {
                                println!("✅ PASSED");
                            } else {
                                println!("❌ FAILED");
                                verification_failed = true;
                                failed_files.push(FileVerificationResult {
                                    filename: f.rfilename.clone(),
                                    expected_hash: lfs_info.oid.clone(),
                                    actual_hash,
                                    passed: false,
                                });
                                
                                // Delete the corrupted file
                                if let Err(e) = fs::remove_file(&local_path).await {
                                    println!("   Failed to delete corrupted file: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            println!("❌ Error computing hash: {}", e);
                        }
                    }
                }
            }
        }
        
        if verification_failed {
            println!("\n⚠️  Hash verification failed for {} LFS file(s):", failed_files.len());
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            for result in &failed_files {
                println!("❌ {}", result.filename);
                println!("   Expected: {}", result.expected_hash);
                println!("   Actual:   {}", result.actual_hash);
                println!("   🗑️  File deleted, please re-run to download");
            }
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("\n🔄 Please run the program again to re-download the corrupted files.");
            
            return Err("Some LFS files failed hash verification. Please run the program again.".into());
        } else {
            let lfs_count = files.iter().filter(|f| f.lfs.is_some()).count();
            if lfs_count > 0 {
                println!("\n✅ All {} LFS files passed hash verification!", lfs_count);
            } else {
                println!("\n✅ No LFS files to verify");
            }
        }
    } else {
        println!("\n⚠️  Hash verification skipped (--skip-sha flag was used)");
    }
    
    Ok(())
}