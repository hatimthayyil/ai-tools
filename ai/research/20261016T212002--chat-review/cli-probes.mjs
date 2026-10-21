import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import cp from 'node:child_process';

const root=fs.mkdtempSync('/tmp/chat-cli-probe-');
const repo=path.join(root,'repo');
for(const d of ['repo','home','claude','bin']) fs.mkdirSync(path.join(root,d));
const bash=cp.execFileSync('which',['bash'],{encoding:'utf8'}).trim();
const fake=path.join(root,'bin/claude');
fs.writeFileSync(fake,`#!${bash}\nprintf '%s\\n' "$@" > '${root}/argv'\nexit 0\n`,{mode:0o755});
const env={...process.env,HOME:path.join(root,'home'),CLAUDE_CONFIG_DIR:path.join(root,'claude'),PATH:path.join(root,'bin')+':'+process.env.PATH,GIT_CONFIG_NOSYSTEM:'1'};
for(const k of ['CLAUDE_CODE_SESSION_ID','CLAUDECODE','GIT_DIR','GIT_AUTHOR_NAME','GIT_COMMITTER_NAME']) delete env[k];
const ai=(args,session)=>cp.spawnSync('/tmp/ai-chat-review-cli-binary',['chat',...args],{cwd:repo,env:{...env,...session?{CLAUDE_CODE_SESSION_ID:session}:{}},encoding:'utf8'});
cp.execFileSync('git',['init','-q',repo],{env});
cp.execFileSync('git',['config','ai.chat.summarise','false'],{cwd:repo,env});
function talk(session,n,prefix,wide=0) {
 let parent=null, rows=[];
 for(let i=0;i<n;i++) {
  const u=`${session}-u${i}`,a=`${session}-a${i}`;
  rows.push({type:'user',uuid:u,parentUuid:parent,timestamp:'2026-10-16T01:00:00Z',cwd:repo,origin:{kind:'human'},message:{role:'user',content:`${prefix} question ${i} `+'q'.repeat(wide)}});
  rows.push({type:'assistant',uuid:a,parentUuid:u,timestamp:'2026-10-16T01:00:00Z',cwd:repo,message:{role:'assistant',model:'claude-opus',content:[{type:'text',text:`${prefix} reply ${i} `+'r'.repeat(wide)}]}});
  parent=a;
 }
 return rows.map(x=>JSON.stringify(x)).join('\n')+'\n';
}
function transcript(session,txt) {
 const file=path.join(root,'claude','projects',repo.replace(/[^A-Za-z0-9]/g,'-'),session+'.jsonl');
 fs.mkdirSync(path.dirname(file),{recursive:true}); fs.writeFileSync(file,txt); return file;
}
function show(label,result) {console.log(label,JSON.stringify({status:result.status,stdout:result.stdout,stderr:result.stderr}));}

transcript('s1',talk('s1',2,'OLD'));
show('attach',ai(['attach','dev'],'s1'));
transcript('s1',talk('s1',3,'OLD'));
show('grep-before-view',ai(['grep','question 2'],'s1'));
show('zoom-before-view',ai(['zoom','2.0'],'s1'));
show('view-syncs',ai(['view'],'s1'));
show('grep-after-view',ai(['grep','question 2'],'s1'));
transcript('s1',talk('s1',4,'OLD'));
show('detach',ai(['detach'],'s1'));
show('sync-after-detach',ai(['sync-session'],'s1'));
show('grep-after-detach',ai(['grep','-t','dev','question 3']));
show('start-newest',ai(['start','dev']));
const started=fs.readFileSync(path.join(root,'argv'),'utf8').trim().split('\n')[1];
transcript(started,'');
show('resume-ignores-newest',ai(['resume','dev']));
console.log('STARTED_SESSION',started,'RESUMED_ARGV',fs.readFileSync(path.join(root,'argv'),'utf8').trim());

transcript('pages1',talk('pages1',64,'OLD-PAGE',180));
show('attach-pages1',ai(['attach','paging'],'pages1'));
transcript('pages2',talk('pages2',1,'NEW-PAGE',20));
show('attach-pages2',ai(['attach','paging'],'pages2'));
const first=ai(['view'],'pages1');
console.log('PAGED_FIRST_STATUS',first.status,'LEN',first.stdout.length,'FOOTER',first.stdout.trim().split('\n').at(-1));
const next=first.stdout.trim().split('\n').at(-1).split('Next: ai chat ')[1];
if(next) {
 const args=next.split(' ');
 const inside=ai(args,'pages1'),outside=ai(args);
 console.log('VIEW_NEXT_SAME_COMMIT_DIFFERENT_BRANCH',JSON.stringify({insideStatus:inside.status,outsideStatus:outside.status,equal:inside.stdout===outside.stdout,insideLen:inside.stdout.length,outsideLen:outside.stdout.length,insideTail:inside.stdout.slice(-400),outsideTail:outside.stdout.slice(-400),outsideErr:outside.stderr}));
}
console.log('ROOT',root);
transcript('large',talk('large',2,'LARGE',2000));
show('attach-large',ai(['attach','modelread'],'large'));
fs.rmSync(path.join(root,'argv'),{force:true});
const read=ai(['view','-t','modelread']);
console.log('VIEW_WITH_SUMMARISE_FALSE_CALLS_MODEL',JSON.stringify({status:read.status,stderr:read.stderr,argv:fs.existsSync(path.join(root,'argv'))?fs.readFileSync(path.join(root,'argv'),'utf8').trim():null,partial:read.stdout.includes('LARGE question 0')}));
fs.rmSync(path.join(root,'argv'),{force:true});
const dry=ai(['summarise','-t','modelread','--dry-run']);
console.log('DRY_RUN_CALLS_MODEL',JSON.stringify({status:dry.status,stdout:dry.stdout,stderr:dry.stderr,argv:fs.existsSync(path.join(root,'argv'))?fs.readFileSync(path.join(root,'argv'),'utf8').trim():null}));
