use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use serde_json::{*};
use crate::taskwait;
use crate::async_taskwait::AsyncTaskWait;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub max_parallel: u32,
	pub max_chunk: u32,
	pub repo_type: String
}

#[derive(Clone, Debug)]
pub struct AppContext {
    pub is_running: Arc<Mutex<bool>>,
    pub json: Arc<Mutex<Option<Value>>>,
	pub config: Option<AppConfig>,
	pub taskwait: Arc<Mutex<Option<AsyncTaskWait>>>
}

impl AppContext {
    pub fn new() -> Self {
        AppContext { is_running: Arc::new(Mutex::new(false)), json: Arc::new(Mutex::new(None)), config: None,
			taskwait: Arc::new(Mutex::new(None))
		}
    }
	
	pub fn run(&mut self) {
		*self.is_running.lock().unwrap() = true;
	}
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            max_parallel: 1,
			max_chunk: 4,
			repo_type: "models".to_string()
        }
    }
}

pub fn set_config(ctx: &mut AppContext) {
    let config_path = format!("{}", get_prog_name());
    
    let config = if fs::metadata(config_path.as_str()).is_ok() {
        // Config file exists, try to load it
        match fs::read_to_string(config_path.as_str()) {
            Ok(content) => {
                match serde_json::from_str::<AppConfig>(&content) {
                    Ok(mut config) => {
                        config
                    },
                    Err(e) => {
                        create_default_config(config_path.as_str())
                    }
                }
            },
            Err(e) => {
                create_default_config(config_path.as_str())
            }
        }
    } else {
        create_default_config(config_path.as_str())
    };
    
    ctx.config = Some(config);
}

fn create_default_config(config_path: &str) -> AppConfig {
    let default_config = AppConfig::default();
    
    match serde_json::to_string_pretty(&default_config) {
        Ok(json_str) => {
			//fs::write(config_path, json_str);
        },
        Err(e) => {
        }
    }
    
    default_config
}

pub fn get_prog_name() -> String {
    let prog_names: Vec<String> = std::env::args().collect();
    let mut prog_name = "config".to_string();
    if prog_names.len() > 0 {
        let stem = Path::new(&prog_names[0])
			.file_stem() // returns Option<OsStr>
			.and_then(|s| s.to_str()) // convert OsStr to &str
			.unwrap_or("");
		prog_name = stem.to_string();
    }

    let result = format!("{}.json", prog_name);
    return result;
}

pub fn save_config(config: &AppConfig) -> anyhow::Result<()> {
    match serde_json::to_string_pretty(config) {
        Ok(json_str) => {
            if let Err(x) = fs::write(format!("{}", get_prog_name()), json_str) {
				Err(anyhow::anyhow!(x))
			} else {
				Ok(())
			}
        },
        Err(e) => {
            Err(anyhow::anyhow!(e))
        }
    }
}

impl AppConfig {
    
}