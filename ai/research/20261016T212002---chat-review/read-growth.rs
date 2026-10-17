use ai_tools::chat::Chat;
use std::{fs, process::Command, time::Instant};
fn main(){
 let root=std::env::temp_dir().join(format!("chat-read-growth-{}",std::process::id()));fs::create_dir_all(&root).unwrap();Command::new("git").args(["init","-q"]).arg(&root).status().unwrap();
 let mut data=String::new();let mut parent="null".to_string();let payload="x".repeat(128*1024);
 for n in 0..1000 {let id=format!("e{n}");let msg=if n%2==0 {"\"user\",\"origin\":{\"kind\":\"human\"},\"message\":{\"role\":\"user\",\"content\":\"next\"}".to_string()}else{format!("\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{payload}\"}}]}}")};data+=&format!("{{\"uuid\":\"{id}\",\"parentUuid\":{parent},\"sessionId\":\"s\",\"timestamp\":\"2026-10-16T00:00:00Z\",\"type\":{msg}}}\n");parent=format!("\"{id}\"");}
 let file=root.join("s.jsonl");fs::write(&file,&data).unwrap();println!("source bytes={} temporary repo={}",data.len(),root.display());drop(data);
 let chat=Chat::find(&root).unwrap().unwrap();chat.attach("t","claude","s",Some(&file)).unwrap();let t=chat.thread("t").unwrap();
 for _ in 0..3 {let s=t.snapshot().unwrap();let start=Instant::now();let n=s.heads().unwrap().len();println!("heads: {n} entries, {:?}",start.elapsed());let start=Instant::now();let n=s.tree().unwrap().items().len();println!("tree: {n} entries, {:?}",start.elapsed());let start=Instant::now();let n=chat.list().unwrap().len();println!("list: {n} threads, {:?}",start.elapsed());}
 let proc=fs::read_to_string("/proc/self/status").unwrap();for l in proc.lines().filter(|l|l.starts_with("VmHWM")){println!("{l}")}
}
