use ai_tools::chat::{Item, Kind, SourceId, Tree, Rollup, Source};
fn id(n:usize)->SourceId{SourceId{harness:"test".into(),session:"s".into(),entry:n.to_string()}}
fn add(v:&mut Vec<Item>,p:Option<usize>,k:Kind)->usize{let n=v.len();v.push(Item{id:id(n),parent:p.map(id),ts:"2026-10-16T00:00:00Z".parse().unwrap(),kind:k,raw:String::new()});n}
fn main(){
 let mut v=Vec::new();let u=add(&mut v,None,Kind::User("request".into()));
 let a=add(&mut v,Some(u),Kind::Text("A".repeat(10000)));
 let mut last=add(&mut v,Some(a),Kind::Text("B".repeat(20000)));
 for i in 0..7 {last=add(&mut v,Some(last),Kind::ToolCall{id:format!("c{i}"),name:"test".into(),input:Default::default()});}
 let shared=last;
 let c=add(&mut v,Some(shared),Kind::Text("C".repeat(24000)));
 let tip_a=add(&mut v,Some(c),Kind::User("next".into()));
 let mut last=add(&mut v,Some(shared),Kind::ToolCall{id:"c7".into(),name:"test".into(),input:Default::default()});
 for i in 0..8 {last=add(&mut v,Some(last),Kind::ToolResult{id:format!("c{i}"),output:"x".repeat(4096),is_error:false});}
 let tree=Tree::new(v);let all=Rollup::new(&tree);
 let ba=all.branch(&tree,tip_a);let bb=all.branch(&tree,last);
 println!("shared seq={shared}; A={:?}; B={:?}",ba.parts[0].done(),bb.parts[0].done());
 for (name,b) in [("A",ba),("B",bb)] {for k in b.chunk_keys(&b.parts[0]) {if k.last==shared as u64 {println!("{name}: key={k}, actual source selected globally={:?}",all.source(k));}}}
 let mut va=tree.items()[..=tip_a].to_vec();let aa=Rollup::new(&Tree::new(va.clone()));
 va.truncate(shared+1);va.extend(tree.items()[tip_a+1..].iter().cloned());let bt=Tree::new(va);let br=Rollup::new(&bt);
 for (label,r) in [("branch A independently",aa),("branch B independently",br)] {for(k,s)in r.lines(){if k.level==0 && k.last==shared as u64 {if let Source::Chunk{items,..}=s{println!("{label}: key={k}, items={items:?}");}}}}
}
