import test from 'node:test';
import assert from 'node:assert/strict';
import {workBuddySessionLink,screenSessionLink,agentSessionLink,codeBuddyFolderLink,codegAppLink,openSessionLink,nestedAgentId,sessionBadge,isCodeBuddyVSCodeHost,hasOpenableFolder} from '../src/monitor/session-link.js';
import {sessionHeadline} from '../src/monitor/model.js';
test('session cards prefer the conversation title over a bare status',()=>{
  assert.equal(sessionHeadline({title:'添加下班驾车离场动画'},'running'),'添加下班驾车离场动画');
  assert.equal(sessionHeadline({title:'  ',project:'agent-mac-office',source:'codex'},'running'),'agent-mac-office');
  assert.equal(sessionHeadline(null,'idle'),'空闲');
});
test('session links preserve exact identity and only open WorkBuddy sessions',()=>{
  assert.equal(workBuddySessionLink({source:'workbuddy',sessionId:'test/a ?#'}),'workbuddy://chat/test%2Fa%20%3F%23');
  assert.equal(workBuddySessionLink({source:'workbuddy',agentType:'workbuddy-ai',sessionId:'test/a ?#'}),'workbuddy-ai://chat/test%2Fa%20%3F%23');
  assert.equal(agentSessionLink({source:'workbuddy',agentType:'international',sessionId:'ai-1'}),'workbuddy-ai://chat/ai-1');
  for(const s of [null,{source:'codex',sessionId:'abc'},{source:'workbuddy',sessionId:''}])assert.equal(workBuddySessionLink(s),null);
});
test('only the waiting screen question card links out',()=>{
  const state={mode:'monitor',status:'wait',source:'workbuddy',sessionId:'test-session'};
  assert.equal(screenSessionLink(state,{x:(14+200*484/512)/512,y:(18+150*232/288)/288}),'workbuddy://chat/test-session');
  assert.equal(screenSessionLink(state,{x:(14+481*484/512)/512,y:(18+262*232/288)/288}),'workbuddy://chat/test-session');
  for(const uv of [null,{x:.1,y:.5},{x:.5,y:.95}])assert.equal(screenSessionLink(state,uv),null);
  assert.equal(screenSessionLink({...state,status:'done'},{x:.5,y:.5}),null);
});

test('Codex links target the exact task from both running and completed screens',()=>{
 assert.equal(agentSessionLink({source:'codex',sessionId:'task/a ?#'}),'codex://threads/task%2Fa%20%3F%23');
 for(const session of [null,{source:'other',sessionId:'a'},{source:'codex',sessionId:' '}])assert.equal(agentSessionLink(session),null);
 for(const status of ['running','done','wait'])assert.equal(screenSessionLink({mode:'monitor',source:'codex',sessionId:'task-1',status},{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'codex://threads/task-1');
});

test('Codeg links target the exact session',()=>{
 assert.equal(codegAppLink({source:'codeg',sessionId:'106'}),'codeg://session/106');
 assert.equal(agentSessionLink({source:'codeg',sessionId:'214'}),'codeg://session/214');
 assert.equal(agentSessionLink({source:'codeg',sessionId:' '}),null);
 assert.equal(codegAppLink({source:'codeg',sessionId:' task/a ?# '}),'codeg://session/task%2Fa%20%3F%23');
 for(const session of [null,{source:'codex',sessionId:'214'},{source:'codeg'}])assert.equal(codegAppLink(session),null);
 const state={mode:'monitor',status:'running',source:'codeg',sessionId:'214'};
 assert.equal(screenSessionLink(state,{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'codeg://session/214');
});

test('legacy links POST to the monitor and session deep links use the system handler',async()=>{
 const calls=[];
 await openSessionLink('/api/open-session?source=codeg&sessionId=214',(url,opts)=>{calls.push({url,opts});return Promise.resolve({ok:true});});
 assert.deepEqual(calls,[{url:'/api/open-session?source=codeg&sessionId=214',opts:{method:'POST'}}]);
 const previous=globalThis.window;
 globalThis.window={location:{href:''}};
 openSessionLink(agentSessionLink({source:'codeg',sessionId:'214'}));
 assert.equal(globalThis.window.location.href,'codeg://session/214');
 openSessionLink('codex://threads/task-1');
 assert.equal(globalThis.window.location.href,'codex://threads/task-1');
 globalThis.window=previous;
});

test('Codeg badges surface the nested agent on the outer portrait',()=>{
  assert.equal(nestedAgentId('codex'),'codex');
  assert.equal(nestedAgentId('Grok'),'grok');
  assert.equal(nestedAgentId('code_buddy'),'codebuddy-ide');
  assert.equal(nestedAgentId('codebuddy-code'),'codebuddy-ide');
  assert.equal(nestedAgentId('claude-code'),'claude');
  assert.equal(nestedAgentId(''),null);
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'codex'}),{host:'codeg',id:'codex',label:'Codex'});
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'grok'}),{host:'codeg',id:'grok',label:'Grok'});
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'code_buddy'}),{host:'codeg',id:'codebuddy-ide',label:'CodeBuddy'});
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'antigravity'}),{host:'codeg',id:'antigravity',label:'antigravity'});
  assert.deepEqual(sessionBadge({source:'codeg'}),{host:'codeg',id:'codeg',label:'Codeg'});
  // A delegated child is named as one and points at the session it came from.
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'code_buddy',subagent:true,parentTitle:'Build feature'}),{host:'codeg',id:'codebuddy-ide',label:'子任务',detail:'父会话：Build feature'});
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'codex',subagent:true,parentTitle:'Build feature'}),{host:'codeg',id:'codex',label:'子任务',detail:'父会话：Build feature'});
  // An unreadable parent title drops the detail, never the 子任务 label.
  assert.deepEqual(sessionBadge({source:'codeg',agentType:'codex',subagent:true}),{host:'codeg',id:'codex',label:'子任务',detail:undefined});
  // The badge id still names the nested agent, so its icon survives.
  assert.equal(sessionBadge({source:'codeg',agentType:'code_buddy',subagent:true}).id,'codebuddy-ide');
  assert.deepEqual(sessionBadge({source:'codex'}),{host:'codex',id:'codex',label:'Codex'});
  assert.deepEqual(sessionBadge({source:'workbuddy'}),{host:'workbuddy',id:'workbuddy',label:'WorkBuddy'});
  assert.deepEqual(sessionBadge({source:'workbuddy',agentType:'workbuddy-ai'}),{host:'workbuddy',id:'workbuddy',label:'WorkBuddy 国际版'});
  assert.deepEqual(sessionBadge({source:'codebuddy-ide'}),{host:'codebuddy-ide',id:'codebuddy-ide',label:'CodeBuddy'});
  assert.deepEqual(sessionBadge({source:'codebuddy-ide',agentType:'codebuddy'}),{host:'codebuddy-ide',id:'codebuddy-ide',label:'CodeBuddy 国际版'});
  assert.deepEqual(sessionBadge({source:'codebuddy-ide',agentType:'codebuddycn'}),{host:'codebuddy-ide',id:'codebuddy-ide',label:'CodeBuddy 国内版'});
  assert.equal(sessionBadge(null),null);
});

test('CodeBuddy IDE links open the project folder, not a conversation id',()=>{
 assert.equal(codeBuddyFolderLink('/Users/apple/Work/a b?'),'codebuddy://file/Users/apple/Work/a%20b%3F');
 assert.equal(agentSessionLink({source:'codebuddy-ide',sessionId:'conv-1',cwd:'/Users/apple/CodeBuddy/Claw'}),'codebuddy://file/Users/apple/CodeBuddy/Claw');
 assert.equal(agentSessionLink({source:'codebuddy-ide',agentType:'codebuddy',cwd:'/Users/apple/CodeBuddy/Claw'}),'codebuddy://file/Users/apple/CodeBuddy/Claw');
 assert.equal(agentSessionLink({source:'codebuddy-ide',agentType:'codebuddycn',cwd:'/Users/apple/CodeBuddy/Claw'}),'codebuddycn://file/Users/apple/CodeBuddy/Claw');
 assert.equal(agentSessionLink({source:'codebuddy-ide',agentType:'domestic',cwd:'/Users/apple/Work/app'}),'codebuddycn://file/Users/apple/Work/app');
 for(const cwd of [undefined,'',' ','/','///','relative/path']){assert.equal(codeBuddyFolderLink(cwd),null);assert.equal(agentSessionLink({source:'codebuddy-ide',sessionId:'conv-1',cwd}),'/api/open-session?source=codebuddy-ide&edition=international');}
 assert.equal(agentSessionLink({source:'codebuddy-ide',agentType:'codebuddycn'}),'/api/open-session?source=codebuddy-ide&edition=domestic');
 assert.equal(agentSessionLink({source:'codex',cwd:'/tmp'}),null);
 const state={mode:'monitor',status:'running',source:'codebuddy-ide',sessionId:'conv-1',cwd:'/work/app'};
 assert.equal(screenSessionLink(state,{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'codebuddy://file/work/app');
 assert.equal(screenSessionLink({...state,agentType:'codebuddycn'},{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'codebuddycn://file/work/app');
});

test('the VS Code plugin keeps the IDE source but becomes a VS Code host',()=>{
 assert.equal(isCodeBuddyVSCodeHost({source:'codebuddy-ide',hostKind:'vscode'}),true);
 for(const s of [null,{source:'codex',hostKind:'vscode'},{source:'codebuddy-ide'},{source:'codebuddy-ide',hostKind:'codebuddy-ide'},{source:'codebuddy-ide',hostKind:'cursor'}])assert.equal(isCodeBuddyVSCodeHost(s),false);
 assert.deepEqual(sessionBadge({source:'codebuddy-ide',hostKind:'vscode'}),{host:'codebuddy-ide',id:'codebuddy-vscode',label:'VS Code',avatar:'codebuddy-vscode'});
 assert.deepEqual(sessionBadge({source:'codebuddy-ide',hostKind:'vscode',agentType:'codebuddycn'}),{host:'codebuddy-ide',id:'codebuddy-vscode',label:'VS Code',avatar:'codebuddy-vscode'});
 for(const cwd of ['/Users/apple/Work/a b?','/work/app'])assert.equal(hasOpenableFolder(cwd),true);
 for(const cwd of [undefined,'',' ','/','///','relative/path'])assert.equal(hasOpenableFolder(cwd),false);
 // VS Code's URL handler always targets the last active window, so the session
 // opens through a desktop command that resolves the folder from the session id.
 assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'vscode',sessionId:'conv-1',cwd:'/Users/apple/CodeBuddy/Claw'}),'/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1&cwd=%2FUsers%2Fapple%2FCodeBuddy%2FClaw');
 assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'vscode',agentType:'codebuddycn',sessionId:'conv-1',cwd:'/work/app'}),'/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1&cwd=%2Fwork%2Fapp');
 assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'vscode',sessionId:'task/a ?#',cwd:'/work/app'}),'/api/open-session?source=codebuddy-ide&host=vscode&session=task%2Fa%20%3F%23&cwd=%2Fwork%2Fapp');
 for(const cwd of [undefined,'',' ','/','///','relative/path'])assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'vscode',sessionId:'conv-1',cwd}),'/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1');
 for(const sessionId of [undefined,'',' '])assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'vscode',sessionId,cwd:'/work/app'}),'/api/open-session?source=codebuddy-ide&host=vscode');
 const state={mode:'monitor',status:'running',source:'codebuddy-ide',hostKind:'vscode',sessionId:'conv-1',cwd:'/work/app'};
 assert.equal(screenSessionLink(state,{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1&cwd=%2Fwork%2Fapp');
 assert.equal(screenSessionLink({...state,cwd:'/'},{x:(14+410*484/512)/512,y:(18+262*232/288)/288}),'/api/open-session?source=codebuddy-ide&host=vscode&session=conv-1');
});

test('a missing or unknown hostKind behaves exactly like a CodeBuddy IDE session',()=>{
 for(const hostKind of [undefined,'codebuddy-ide','','cursor']){
  const s={source:'codebuddy-ide',sessionId:'conv-1',cwd:'/work/app',...(hostKind===undefined?{}:{hostKind})};
  assert.deepEqual(sessionBadge(s),{host:'codebuddy-ide',id:'codebuddy-ide',label:'CodeBuddy'});
  assert.equal(agentSessionLink(s),'codebuddy://file/work/app');
  assert.deepEqual(sessionBadge({...s,agentType:'codebuddycn'}),{host:'codebuddy-ide',id:'codebuddy-ide',label:'CodeBuddy 国内版'});
  assert.equal(agentSessionLink({...s,agentType:'codebuddycn'}),'codebuddycn://file/work/app');
 }
 assert.equal(agentSessionLink({source:'codebuddy-ide',hostKind:'codebuddy-ide'}),'/api/open-session?source=codebuddy-ide&edition=international');
});
