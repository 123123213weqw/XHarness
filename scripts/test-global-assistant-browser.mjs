/** Regression for the old global-assistant entry using compiled Layout and the normal chat.
 * Historical chat fixture; no live Host, repositories, or model costs. */
import assert from 'node:assert/strict'
import {createRequire} from 'node:module'
import {resolve} from 'node:path'
import {mkdirSync,readFileSync} from 'node:fs'
import {installOwnedViewHtml} from './fixtures/owned-view-platform-browser.mjs'
import {compileSourceModules} from './build-source-modules.mjs'
const require=createRequire(resolve(process.env.UI_TEST_DEPS??'/Users/wangyue/codex-build/xharness-plugin-migration/ui-browser-deps','package.json'))
const {chromium,webkit}=require('playwright'),engine=process.env.UI_TEST_BROWSER??'chromium'
const compiled=compileSourceModules(new URL('../ui',import.meta.url).pathname,[{id:'sidebar-test',source:'src/modules/sidebar/SidebarRoot.tsx'},{id:'settings-test',source:'src/modules/settings-general/SettingsRoot.tsx'},{id:'frame-test',source:'src/modules/layout/AppFrame.tsx'},{id:'navigation-test',source:'src/modules/layout/shell-navigation.ts'},{id:'@xharness/dsh-client-ui-conversation',source:'src/modules/conversation/index.ts'},{id:'@xharness/dsh-client-ui-conversation/test',source:'src/modules/conversation/test-exports.ts'},{id:'tool-test',source:'src/modules/tool/test-exports.ts'},{id:'model-test',source:'src/modules/model-selection/ModelSelect.tsx'},{id:'model-locales',source:'src/modules/model-selection/locales.ts'},{id:'path-test',source:'src/modules/client-runtime/workspaces/path.ts'}])
const browser=await({chromium,webkit}[engine]).launch({headless:true})
try {
 const page=await browser.newPage({viewport:{width:1700,height:950},locale:'en-US'}),errors=[];page.on('pageerror',e=>{if(e.message!=='owned feature fixture: stop Host boot')errors.push(e.message);console.error('browser-error:',e.message)});page.setDefaultTimeout(15000)
 await installOwnedViewHtml(page,'source','<html lang="en"><head></head><body style="margin:0"><div id="root" style="position:fixed;inset:0"></div></body></html>',{origin:'http://retired-shell.test'})
 await page.evaluate(()=>window.__ModuleLoader__={load:r=>window.registration=r})
 for(const [name,source] of [['sidebarRegistration',compiled.get('sidebar-test').bytes.toString()],['settingsRegistration',compiled.get('settings-test').bytes.toString()],['frameRegistration',compiled.get('frame-test').bytes.toString()],['navigationRegistration',compiled.get('navigation-test').bytes.toString()],['conversationRegistration',compiled.get('@xharness/dsh-client-ui-conversation/test').bytes.toString()],['toolRegistration',compiled.get('tool-test').bytes.toString()],['modelRegistration',compiled.get('model-test').bytes.toString()],['modelLocales',compiled.get('model-locales').bytes.toString()],['pathRegistration',compiled.get('path-test').bytes.toString()]]){await page.addScriptTag({content:source});await page.evaluate(name=>window[name]=registration,name)}
 await page.evaluate(()=>{
  const jsx=React.createElement
  const makeStore=initial=>{let value=initial;const listeners=new Set();return{getSnapshot:()=>value,subscribe:f=>{listeners.add(f);return()=>listeners.delete(f)},set:next=>{value=next;for(const f of listeners)f()},update:f=>{value={...value};f(value);for(const fn of listeners)fn()}}}
  const hook=store=>select=>select(React.useSyncExternalStore(store.subscribe,store.getSnapshot))
  const runtime={...pathRegistration.factory(),createSnapshotStore:makeStore,defineStore:spec=>({spec}),workspaceTitleOf:path=>path.split('/').at(-1),publishChatSnapshot:v=>v,shallowEqual:(a,b)=>a===b}
  const resolver=id=>id in staticModules?staticModules[id]:id.startsWith('@xharness/dsh-client-runtime')?runtime:id.startsWith('@xharness/cordis')?{Service:class{constructor(ctx){this.ctx=ctx}},Context:{is:()=>false}}:(()=>{throw Error(id)})()
  window.plugin=conversationRegistration.factory(resolver)
  window.tool=toolRegistration.factory(resolver);window.model=modelRegistration.factory(resolver);window.modelLabels=modelLocales.factory(resolver).en
  const {AppFrame}=frameRegistration.factory(resolver);const {SidebarRoot}=sidebarRegistration.factory(resolver);const {SettingsRoot}=settingsRegistration.factory(resolver);const {IconSettingsOutline16}=resolver('@xharness/dsh-client-ui-primitives')
  window.calls=[];window.mounts=0;window.unmounts=0;window.created=[];window.opened=[];window.assistantId='xharness-global-assistant-v1'
  const rows={s1:{id:'s1',displayTitle:'Existing task',blank:false,cwd:'/repo',running:false},s2:{id:'s2',displayTitle:'Another task',blank:false,cwd:'/repo',running:true},[assistantId]:{id:assistantId,displayTitle:'Former assistant history',blank:false,cwd:'/repo',running:false}}
  window.summaries=makeStore({ids:Object.keys(rows),byId:rows,current:'s1',phase:'ready',jobsBySession:{}})
  const workspaces=makeStore({items:[{workspaceId:'w',title:'Repo',path:'/repo',sessionIds:['s1','s2',assistantId]}],phase:'ready'})
  const shells=new Map();window.shells=shells
  function input(id){let shell=shells.get(id);if(!shell){shell=new plugin.SessionInputShell({actx:{},defaultSink:async(text,images,mode)=>{calls.push({sessionId:id,text,images,mode});return{kind:'success'}},commandImages:{serialize:async()=>[],release(){},unsupportedNotice:()=>''}});shells.set(id,shell)}return shell}
  input('s1').setDraft('Other task draft');input(assistantId).setDraft('Keep this draft')
  window.sessions={
   clear:()=>summaries.set({...summaries.getSnapshot(),current:undefined}),
   subagentAddress:()=>undefined,list:summaries,
   open:id=>{assertListed(id);opened.push(id);summaries.set({...summaries.getSnapshot(),current:id})},
   create:async args=>{created.push(args);throw Error('retired entry must not create a session')},
  }
  function assertListed(id){if(!summaries.getSnapshot().ids.includes(id))throw Error('missing chat')}
  window.navigation=new (navigationRegistration.factory().ShellNavigation)(sessions)
  const layout=makeStore({sidebar:280,details:0});window.navLayout=layout;const useSessions=hook(summaries),useWorkspaces=hook(workspaces)
  const translate=labels=>(key,args)=>{let value=labels[key]??key;for(const [k,v] of Object.entries(args??{}))value=value.replaceAll('{'+k+'}',String(v));return value}
  const t=translate(plugin.en),mt=translate(modelLabels),views={subscribe:()=>()=>{},version:()=>0,list:()=>[{id:'chat',label:'Chat'}]}
  const snapshots=new Map(),viewStores=new Map(),directories=new Map(),scrollPositions=new Map();window.snapshots=snapshots
  function stateFor(id){if(!snapshots.has(id)){
   const nodes=new Map(),location={kind:'session'},now=Date.now();const add=(key,kind,data)=>nodes.set(key,{key,kind,data,location})
   add('user','user',{kind:'user',seq:1,time:now,source:{kind:'user'},content:[{type:'text',text:`Review history for ${id}: please inspect this change.`}]})
   add('answer','assistant-step',{status:'settled',turn:0,step:0,time:now,blocks:[{kind:'reasoning',text:'Compare the requested scope with the selected commit.'},{kind:'text',text:`Existing answer for ${id}. This is controlled test data, not a real review verdict.`}]})
   add('tool','tool-call',{root:{kind:'tool-result',seq:3,time:now,callId:'read-'+id,callView:null,call:{name:'read',argsRaw:'{"path":"a.rs"}'},callTime:now-100,content:[{type:'text',text:'Existing tool result for '+id}],isError:false,resultView:null,subCalls:[]}})
   snapshots.set(id,makeStore({openState:'open',blank:false,composerPhase:'active',running:false,removed:false,hasMore:true,loadingOlder:false,pending:[],queue:[],chat:{order:[...nodes.keys()],nodes,timeline:{turns:new Map(),steps:new Map()}}}))
   viewStores.set(id,makeStore({view:'chat',draft:'',selection:null,inspect:null}))
   directories.set(id,makeStore({current:{provider:'fixture',model:'test',reasoningEffort:'high'},routable:true,status:'ready',error:null,failures:[],groups:[{id:'fixture',name:'Test provider',models:[{id:'test',name:'Test model',reasoning:{defaultEffort:'high',efforts:[{id:'low',name:'Low'},{id:'high',name:'High'}]}}]}]}))
  }return snapshots.get(id)}
  window.NativeChat=function NativeChat(){const id=useSessions(s=>s.current),shell=input(id),useInput=hook(shell.state),snapshot=stateFor(id),useSession=hook(snapshot),viewStore=viewStores.get(id),directory=directories.get(id),useStore=hook(viewStore)
   React.useEffect(()=>{mounts++;return()=>{unmounts++}},[])
   const actions={setView:view=>viewStore.set({...viewStore.getSnapshot(),view}),setInspect:inspect=>viewStore.set({...viewStore.getSnapshot(),inspect})}
   const inspectCall=callId=>{calls.push({inspect:callId,sessionId:id});viewStore.set({...viewStore.getSnapshot(),selection:{callId}})}
   const nodeProps={t,useTurnData:()=>undefined,editAvailable:false,editMessage:()=>{},forkMessage:async()=>{},openFile:async()=>{},forkAt:async()=>{},inspectCall,fileMentions:()=>undefined,renderMessageImages:()=>null}
   const renderNode=(key,owner,options)=>{
    if(key==='conversation.chat.node'){const Component={user:plugin.UserMessageNodeView,'assistant-step':plugin.AssistantNodeView,'tool-call':tool.ToolCallTree}[owner.node.kind];return Component?jsx(Component,{...nodeProps,...owner,useHostDescription:select=>select({home:'/home/test'}),renderSlot:(_key,_owner,opts)=>opts.fallback}):options?.fallback}
    return null
   }
   const renderSlot=(key,owner)=>{
    if(key==='conversation.composer.bar')return jsx(plugin.InputBar,{sessionId:id,useSession,useInput,inputActions:shell.actions,keyboard:shell,draftImages:()=>[],addImages:()=>null,removeImage:()=>{},resolveSubmitMode:()=> 'queue',toggleCommandMenu:()=>{},stop:()=>{},command:async()=>false,useNotices:hook(shell.notices),useLexicon:hook(shell.lexicon),useMenuLauncher:()=>false,useProjection:()=>undefined,t,renderSlot:(key,owner)=>key==='conversation.input.model'?jsx(model.XHarnessModelSelect,{available:true,locked:owner.locked,directory,t:mt,load:()=>{},select:async current=>{calls.push({model:current,sessionId:id});directory.set({...directory.getSnapshot(),current});return true}}):null,...owner})
    if(key==='conversation.session.header')return jsx(plugin.ConversationSessionHeader,{sessionId:id,useSession,useSessions,useStore,actions,views,open:sessions.open,t,renderSlot:()=>null})
    if(key==='conversation.session')return jsx(plugin.ConversationSession,{sessionId:id,useSession,useInput,inputActions:shell.actions,useStore,actions,views,bindDraftMirror:()=>()=>{},releaseSessionImages:()=>{},renderSlot:()=>jsx(plugin.ChatView,{sessionId:id,useSession,useSessions,useStore,...nodeProps,loadImage:async()=>'',loadOlder:()=>calls.push({older:id}),chatScroll:{read:()=>scrollPositions.get(id)??null,save:p=>scrollPositions.set(id,p)},renderSlot:renderNode})})
    return null
   }
   return jsx(plugin.ConversationRoot,{sessionId:id,useSession,useSessions,useWorkspaces,useInput,useComposerBlock:()=>undefined,selectWorkspace:async()=>{},t,renderSlot,renderSlotChain:(_key,_owner,options)=>options.fallback})
  }
  function renderSlot(key,owner){return key==='conversation'?jsx(NativeChat,{}):key==='sidebar'?jsx(SidebarRoot,{...owner,startSession:()=>calls.push({newSession:true}),toggleSidebar:()=>layout.set({...layout.getSnapshot(),sidebar:layout.getSnapshot().sidebar?0:280}),t:key=>({'session.new.label':'New session','session.new':'New Session','navigation.back':'Back','navigation.forward':'Forward','toggle.open':'Open sidebar','toggle.collapse':'Collapse sidebar'}[key]??key),renderSlot:(key,owner,options)=>key==='sidebar.footer.action'?jsx(React.Fragment,null,jsx('button',{'data-footer-peer':'',type:'button',style:{boxSizing:'border-box',width:owner.wide?'100%':36,height:36,flex:'none'}},owner.wide?'Plugin activity':'·')):key==='sidebar.workspaces'?jsx('div',null,[...summaries.getSnapshot().ids].map(id=>jsx('button',{type:'button',key:id,onClick:()=>sessions.open(id)},summaries.getSnapshot().byId[id].displayTitle))):key==='sidebar.settings'?jsx(SettingsRoot,{...owner,useSections:()=>[],useOnboardingSteps:()=>[],useSessions,renderSlot:(slot,{wide}={},options)=>slot==='settings.trigger'?jsx(React.Fragment,null,jsx(IconSettingsOutline16,{size:wide?16:18}),wide&&jsx('span',{className:'_8OspXW_triggerLabel'},'Settings')):options?.fallback??null}):options?.fallback??null}):null}
  const root=ReactDOM.createRoot(document.getElementById('root'));window.closeRoot=()=>root.unmount();root.render(jsx(AppFrame,{navigation,useSessions,useStore:hook(layout),actions:{closeDetails(){},toggleSidebar(){},setNarrow(){},setSidebar(){}},renderSlot}))
 })
 await page.locator('textarea').waitFor().catch(async e=>{console.error((await page.locator('body').innerText()).slice(0,4000));throw e});const resident=await page.locator('.xhwork-conversation').elementHandle(),composer=await page.locator('textarea').elementHandle()

 const palette=await page.addStyleTag({content:readFileSync('ui/src/modules/theme/design-platform.css','utf8')+'\n'+readFileSync('ui/overrides/monochrome.css','utf8')})
 // The retired action stays absent at every sidebar width and palette; peers remain usable.
 for(const dark of [false,true])for(const width of [264,320,420]) {
  await page.evaluate(({dark,width})=>{document.body.toggleAttribute('data-ds-dark-theme',dark);navLayout.set({...navLayout.getSnapshot(),sidebar:width})},{dark,width})
  await page.waitForFunction(width=>document.querySelector('._84hhiq_frame').style.gridTemplateColumns.startsWith(`${width}px `),width)
  assert.equal(await page.getByRole('button',{name:'Code Review',exact:true}).count(),0)
  assert.equal(await page.locator('[data-xharness-review-nav],[data-xharness-assistant-nav]').count(),0)
  assert.equal(await page.locator('.xhsidebar-primary-actions').getByRole('button',{name:'New session',exact:true}).isVisible(),true)
  assert.equal(await page.locator('[data-footer-peer]').isVisible(),true)
 }
 await page.evaluate(()=>{document.body.removeAttribute('data-ds-dark-theme');navLayout.set({...navLayout.getSnapshot(),sidebar:280})});await palette.evaluate(node=>node.remove())
 if(process.env.UI_PREVIEW_DIR){mkdirSync(process.env.UI_PREVIEW_DIR,{recursive:true});await page.screenshot({path:resolve(process.env.UI_PREVIEW_DIR,'code-review-retired.png')})}

 // A stale external event must not create/open anything, replace drafts, or
 // disturb an unrelated route. The original review intent still falls back.
 await page.evaluate(()=>window.dispatchEvent(new Event('xharness:plugins:open')))
 await page.getByRole('button',{name:'Back to chat',exact:true}).waitFor()
 await page.evaluate(async()=>{
  window.dispatchEvent(new CustomEvent('xharness:assistant:open',{detail:{key:'old-pr',text:'Do not stage this'}}))
  window.dispatchEvent(new Event('xharness:assistant:close'))
  await Promise.resolve()
 })
 assert.equal(await page.evaluate(()=>navigation.getSnapshot().route.page),'plugins')
 assert.deepEqual(await page.evaluate(()=>({opened,created,calls})),{opened:[],created:[],calls:[]})
 await page.evaluate(()=>window.dispatchEvent(new Event('xharness:review:open')))
 await page.locator('textarea').waitFor()
 assert.equal(await page.locator('textarea').inputValue(),'Other task draft')

 // The fixed historical ID is visible via ordinary workspace selection and
 // reuses the same resident conversation, composer, and input machine.
 await page.getByRole('button',{name:'Former assistant history',exact:true}).click()
 await page.waitForFunction(()=>summaries.getSnapshot().current===assistantId)
 assert.equal(await page.locator('textarea').inputValue(),'Keep this draft')
 assert.equal(await composer.evaluate(n=>n===document.querySelector('textarea')),true)
 await page.getByText('Existing answer for xharness-global-assistant-v1.',{exact:false}).waitFor();await page.locator('[data-chat-flow]').waitFor()
 const reasoningDisclosure=page.locator('[data-disclosure-row][role=button]').filter({hasText:/^Thought/});assert.equal(await reasoningDisclosure.count(),1);await reasoningDisclosure.click();await page.locator('[data-variant="think"] [class$="_thinkBody"]').filter({hasText:'Compare the requested scope with the selected commit.'}).waitFor();assert.equal(await reasoningDisclosure.getAttribute('aria-expanded'),'true')
 await page.locator('[data-tool="read"] [aria-expanded]').first().focus();await page.locator('[data-tool="read"] [aria-expanded]').first().press('Enter');await page.getByText('Existing tool result for xharness-global-assistant-v1',{exact:true}).waitFor()
 await page.getByRole('button',{name:'Inspect',exact:true}).click();assert.equal(await page.evaluate(()=>calls.at(-1).sessionId),'xharness-global-assistant-v1')
 await page.getByRole('button',{name:'Load earlier',exact:true}).click();assert.ok(await page.evaluate(()=>calls.some(c=>c.older===assistantId)))
 await page.getByRole('button',{name:/Select model, current Test model/}).click();await page.getByRole('button',{name:/Select model, current Test model/}).press('Escape');await page.getByRole('button',{name:'Thinking: High',exact:true}).click();await page.getByRole('menuitemradio',{name:'Low',exact:true}).click();await page.getByRole('button',{name:'Thinking: Low',exact:true}).waitFor();assert.equal(await page.evaluate(()=>calls.find(c=>c.model).sessionId),'xharness-global-assistant-v1')
 await page.evaluate(()=>{const s=snapshots.get(assistantId),v=s.getSnapshot(),nodes=new Map(v.chat.nodes),old=nodes.get('answer');nodes.set('answer',{...old,data:{...old.data,status:'running',blocks:[{kind:'reasoning',text:'Compare the requested scope with the selected commit.'},{kind:'text',text:'Streaming update in the original chat.'}]}});s.set({...v,chat:{...v.chat,nodes}});calls.length=0})
 await page.getByText('Streaming update in the original chat.',{exact:true}).waitFor();assert.equal(await page.locator('[data-tool="read"] [aria-expanded]').first().getAttribute('aria-expanded'),'true')
 await page.locator('textarea').press('Enter')
 await page.waitForFunction(()=>calls.length===1)
 assert.deepEqual(await page.evaluate(()=>({id:calls[0].sessionId,text:calls[0].text})),{id:'xharness-global-assistant-v1',text:'Keep this draft'})
 assert.equal(await page.evaluate(()=>shells.get('s1').snapshot.draft),'Other task draft')
 assert.equal(await page.evaluate(()=>mounts),1)
 assert.equal(await page.evaluate(()=>unmounts),0)
 assert.equal(await resident.evaluate(node=>node.isConnected),true)
 await page.setViewportSize({width:680,height:950})
 await page.locator('.xhsidebar-primary-actions[data-wide=false]').waitFor()
 assert.equal(await page.locator('[data-xharness-review-nav],[data-xharness-assistant-nav]').count(),0)
 assert.equal(await page.locator('.xhsidebar-primary-actions').getByRole('button',{name:'New session',exact:true}).isVisible(),true)
 assert.equal(await page.locator('textarea').count(),1)
 // Shell history is UI navigation, not a replay of feature requests or Agent work.
 await page.setViewportSize({width:1700,height:950})
 await page.locator('.xhsidebar-primary-actions[data-wide=true]').waitFor()
 const back=page.getByRole('button',{name:'Back',exact:true}),forward=page.getByRole('button',{name:'Forward',exact:true})
 await page.locator('textarea').fill('Navigation draft stays intact')
 const toolToggle=page.locator('[data-tool="read"] [aria-expanded]').first()
 if(await toolToggle.getAttribute('aria-expanded')!=='true'){await toolToggle.focus();await toolToggle.press('Enter')}
 await page.waitForFunction(()=>document.querySelector('[data-tool="read"] [aria-expanded]')?.getAttribute('aria-expanded')==='true')
 const beforeNavigation=await page.evaluate(()=>({calls:calls.length,created:created.length}))
 await page.evaluate(()=>window.dispatchEvent(new Event('xharness:plugins:open')))
 await page.getByRole('button',{name:'Back to chat',exact:true}).waitFor()
 await back.click();await page.locator('textarea').waitFor()
 assert.equal(await page.locator('textarea').inputValue(),'Navigation draft stays intact')
 assert.equal(await toolToggle.getAttribute('aria-expanded'),'true','returning from a center page preserves expanded tool DOM; '+JSON.stringify(await page.evaluate(()=>({current:summaries.getSnapshot().current,route:navigation.getSnapshot().route,tools:document.querySelectorAll('[data-tool="read"]').length,visible:!document.querySelector('.xhwork-conversation').hidden}))))
 await forward.click();await page.getByRole('button',{name:'Back to chat',exact:true}).waitFor()
 await back.click();await page.locator('textarea').waitFor()
 assert.deepEqual(await page.evaluate(()=>({calls:calls.length,created:created.length})),beforeNavigation)
 // Independent session drafts and running state survive the existing selection port.
 await page.evaluate(()=>{navigation.close();sessions.open('s1')})
 await page.waitForFunction(()=>navigation.getSnapshot().route.page==='chat'&&summaries.getSnapshot().current==='s1')
 await page.locator('textarea').fill('Back draft A')
 await page.evaluate(()=>sessions.open('s2'))
 await page.waitForFunction(()=>summaries.getSnapshot().current==='s2')
 await page.locator('textarea').fill('Forward draft B')
 await back.click();await page.waitForFunction(()=>summaries.getSnapshot().current==='s1')
 assert.equal(await page.locator('textarea').inputValue(),'Back draft A')
 await forward.click();await page.waitForFunction(()=>summaries.getSnapshot().current==='s2')
 assert.equal(await page.locator('textarea').inputValue(),'Forward draft B')
 assert.equal(await page.evaluate(()=>summaries.getSnapshot().byId.s2.running),true,'navigation does not stop the running Agent')
 // macOS physical seat: arrows to the LEFT of the same panel toggle, all
 // outside the draggable strip; no duplicate Web controls after relocation.
 await page.addStyleTag({content:readFileSync('ui/desktop/titlebar.css','utf8')})
 await page.evaluate(()=>{
  document.documentElement.dataset.xhMacTitlebar='overlay'
  const bar=document.createElement('div');bar.id='xh-desktop-titlebar'
  const drag=document.createElement('div');drag.id='xh-desktop-titlebar-drag';drag.setAttribute('data-tauri-drag-region','');bar.append(drag)
  const controls=document.createElement('div');controls.id='xh-desktop-titlebar-controls';bar.append(controls);document.body.prepend(bar)
  window.dispatchEvent(new Event('xh-desktop-titlebar-ready'))
 })
 await page.locator('#xh-desktop-titlebar-controls [data-shell-navigation-back]').waitFor()
 assert.equal(await page.locator('[data-shell-navigation-back]').count(),1)
 assert.equal(await page.locator('[data-shell-navigation-forward]').count(),1)
 const titlebarGeometry=await page.evaluate(()=>{
  const back=document.querySelector('[data-shell-navigation-back]'),forward=document.querySelector('[data-shell-navigation-forward]'),toggle=document.querySelector('.xh-desktop-sidebar-toggle'),drag=document.querySelector('#xh-desktop-titlebar-drag')
  const a=back.getBoundingClientRect(),b=forward.getBoundingClientRect(),c=toggle.getBoundingClientRect(),d=drag.getBoundingClientRect()
  return{ordered:a.right<=b.left&&b.right<=c.left,aligned:a.y===b.y&&b.y===c.y,sameSize:a.width===b.width&&b.width===c.width&&a.height===c.height,trafficSafe:a.left>=88,noOverlap:c.right<=d.left,notDraggable:![back,forward,toggle].some(el=>el.closest('[data-tauri-drag-region]'))}
 })
 assert.deepEqual(titlebarGeometry,{ordered:true,aligned:true,sameSize:true,trafficSafe:true,noOverlap:true,notDraggable:true})
 await page.locator('.xh-desktop-sidebar-toggle').click()
 await page.locator('.xhsidebar-primary-actions[data-wide=false]').waitFor()
 await page.locator('#xh-desktop-titlebar-controls [data-shell-navigation-back]').press('Enter')
 await page.waitForFunction(()=>summaries.getSnapshot().current==='s1')
 if(process.env.UI_PREVIEW_DIR)await page.screenshot({path:resolve(process.env.UI_PREVIEW_DIR,'shell-navigation-'+engine+'.png')})
 await page.evaluate(()=>closeRoot());
 assert.deepEqual(errors,[])
 console.log(JSON.stringify({engine,littleXRetired:true,oldEventsInert:true,legacyChatPreserved:true,normalSend:true,singleOutlet:true,sessionDrafts:true,titlebarGeometry,mobile:true,codeReviewRetired:true}))
}finally{await browser.close()}
