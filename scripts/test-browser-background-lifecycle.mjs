import {installShellSessionsFixture} from './fixtures/shell-navigation-browser.mjs'
import './test-browser-activity.mjs'
// Real AppFrame/BrowserPane; fake ONLY the Tauri transport. Not a native input
// or engine acceptance test. Never equate Playwright WebKit with WKWebView.
import assert from 'node:assert/strict'
import {createRequire} from 'node:module'
import {resolve} from 'node:path'
import {installOwnedViewHtml} from './fixtures/owned-view-platform-browser.mjs'
import {ownedViewModuleTestInput} from './owned-view-module-test-input.mjs'
const deps=resolve(process.env.UI_TEST_DEPS??'/Users/wangyue/codex-build/xharness-plugin-migration/ui-browser-deps')
const require=createRequire(resolve(deps,'package.json'))
const engine=process.env.UI_TEST_BROWSER??'chromium'
const browser=await require('playwright')[engine].launch({headless:true,...(process.env.UI_TEST_EXECUTABLE?{executablePath:process.env.UI_TEST_EXECUTABLE}:{})})
try{
 const page=await browser.newPage({viewport:{width:1400,height:800}})
 const errors=[]
 page.on('pageerror',e=>{if(e.message!=='owned feature fixture: stop Host boot'){errors.push(e.message);console.error('PAGE',e.message)}})
 await installOwnedViewHtml(page,'canonical','<html><body><div id="root" style="position:fixed;inset:0"></div></body></html>')
 await page.addScriptTag({content:`
 window.registrations={};window.__ModuleLoader__={load:x=>registrations[x.id]=x};
 window.calls=[];window.headless=new Map();window.nativeTabs=new Map();window.nativeListeners=new Map();window.activeTab=null;window.restoreDone=false;
 window.emitNative=(event,payload)=>{for(const fn of nativeListeners.get(event)??[])fn({event,payload,id:1})};
 window.__TAURI__={core:{invoke:async(command,args={})=>{
   calls.push({command,args});
   if(command==='desktop_browser_restore'){restoreDone=true;return '{}'}
   if(command==='desktop_headless_control'&&args.arguments.action==='close'){
     if(window.failClose)throw Error('fixture close not confirmed')
     const view=headless.get(args.tabId);if(!view||view.owner!==args.owner)throw Error('wrong close scope')
     headless.delete(args.tabId);return {ok:true,closed:true,tab_id:args.tabId}
   }
   if(command==='desktop_headless_preview'){
     const view=headless.get(args.tabId);if(!view||view.owner!==args.owner)throw Error('wrong headless owner')
     const control={mode:view.mode,generation:String(view.generation),prior_effect_uncertain:false}
     return {tabId:args.tabId,width:640,height:360,mime:'image/jpeg',image:'/9j/',url:view.url,control,...(view.mode==='human'?{manualFrame:'f'.repeat(32)}:{})}
   }
   if(command==='desktop_headless_manual'){
     const view=headless.get(args.tabId), action=args.arguments.action
     if(!view||view.owner!==args.owner||String(view.generation)!==args.arguments.generation)throw Error('wrong control scope')
     if(action==='takeover'||action==='return'){view.mode=action==='takeover'?'human':'agent';view.generation++;return {mode:view.mode,generation:String(view.generation)}}
     if(view.mode!=='human')throw Error('cannot write to a read-only preview')
     return {ok:true,effect:'applied'}
   }
   if(command==='desktop_browser_activate'){activeTab=args.tabId;return nativeTabs.has(args.tabId)}
   if(command==='desktop_browser_page_state')return {...nativeTabs.get(args.tabId)};
   if(command==='desktop_browser_close'){nativeTabs.delete(args.tabId);return}
   if(command==='desktop_browser_navigate'){
     const url=window.redirectNext??args.url;window.redirectNext=undefined;
     nativeTabs.set(args.tabId,{url,loaded:true});
     // Regression: Finished arrives BEFORE the navigate promise resolves.
     emitNative('xharness-browser-event',{tabId:args.tabId,kind:'loading',value:url});
     emitNative('xharness-browser-event',{tabId:args.tabId,kind:'url',value:url});
     emitNative('xharness-browser-event',{tabId:args.tabId,kind:'loaded',value:url});
     // A duplicate URL is not a new load. Previously this poisoned readiness.
     emitNative('xharness-browser-event',{tabId:args.tabId,kind:'url',value:url});return;
   }
   if(command==='desktop_browser_delegate'){
     if(new URL(nativeTabs.get(args.tabId).url).origin!==args.expectedOrigin)throw Error('browser origin changed; synchronize the visible page again');
     return {origin:args.expectedOrigin,grant:{owner:args.owner,allowActions:true,remainingMs:600000}};
   }
   if(command==='desktop_browser_control_reply')throw Error('renderer must not acknowledge service readiness');
 }},event:{listen:async(event,fn)=>{const set=nativeListeners.get(event)??new Set();set.add(fn);nativeListeners.set(event,set);return()=>set.delete(fn)}}};
 `})
 for(const id of ['@xharness/dsh-client-ui-layout','@xlang/xharness-client-ui-browser','@xharness/dsh-client-runtime'])await page.addScriptTag({content:ownedViewModuleTestInput(id)})
  await installShellSessionsFixture(page)
 await page.evaluate(()=>{
   const runtime=registrations['@xharness/dsh-client-runtime'].factory(id=>staticModules[id])
   const load=id=>id==='@xharness/dsh-client-runtime/client'?runtime:staticModules[id]
   const layout=registrations['@xharness/dsh-client-ui-layout'].factory(load)
   const browser=registrations['@xlang/xharness-client-ui-browser'].factory(load)
   browser.apply({get:()=>undefined,remote:{$on:()=>()=>{}},effect:fn=>fn(),slots:{inject:(_name,fn)=>fn(),register:()=>{}}})
    const shellSessions=createShellSessionsFixture({current:'chat-a',ids:['chat-a'],byId:{'chat-a':{blank:false}}})
   let AppFrame,definition
   layout.apply({get:name=>name==='sessions'?shellSessions:undefined,effect:(fn,label)=>{if(label.includes('service'))fn()},reflect:{provide:()=>()=>{}},slots:{register:(spec,view)=>{AppFrame=view;definition=spec;return()=>{}}}})
   const store=definition.store().create();const injected=definition.inject(store.actions)
   function App(){
     const [owner,setOwner]=React.useState('chat-a');window.selectOwner=id=>{shellSessions.update({current:id,ids:[id],byId:{[id]:{blank:false}}});setOwner(id)}
     return React.createElement(React.Fragment,null,React.createElement(browser.BrowserToggle),React.createElement(AppFrame,{...injected,useStore:s=>s(React.useSyncExternalStore(store.subscribe,store.getSnapshot)),useSessions:s=>s({current:owner,byId:{}}),actions:store.actions,
      renderSlot:(name,props)=>name==='workspace.item'?React.createElement(browser.BrowserPane,props):null}))
   }
   window.root=ReactDOM.createRoot(document.getElementById('root'));root.render(React.createElement(App))
 })
 await page.waitForFunction(()=>restoreDone&&nativeListeners.get('xharness-browser-owned-page')?.size===1)
 const aid='browser:'+'a'.repeat(32),bid='browser:'+'b'.repeat(32)
 const serviceOpen=async(owner,tabId,url)=>page.evaluate(({owner,tabId,url})=>{nativeTabs.set(tabId,{url,loaded:true});emitNative('xharness-browser-owned-page',{owner,tabId,url,closed:false})},{owner,tabId,url})
 await serviceOpen('chat-a',aid,'https://example.com/')
 await page.getByRole('textbox',{name:'网址'}).waitFor()
 await page.waitForFunction(id=>activeTab===id,aid)
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_browser_navigate'||c.command==='desktop_browser_control_reply')),false,'UI presents an existing ready page; never creates or acknowledges it')
 await serviceOpen('chat-a',aid,'https://example.com/')
 assert.equal(await page.getByRole('tab').count(),1,'duplicate ready notification cannot duplicate a tab')
 await page.evaluate(()=>selectOwner('chat-b'))
 await page.waitForFunction(()=>activeTab===null)
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_browser_close')),false,'switching chats must not close A')
 const before=await page.evaluate(()=>calls.length)
 // A finishes opening another page while the user is in B. It updates only A.
 await serviceOpen('chat-a','browser:'+'c'.repeat(32),'https://background.example/')
 await page.waitForTimeout(100)
 assert.equal(await page.locator('.xhworkspace-item:not([hidden]) .xhbrowser-pane').count(),0)
 assert.equal(await page.evaluate(start=>calls.slice(start).some(c=>c.command==='desktop_browser_navigate'||c.command==='desktop_browser_delegate'||c.command==='desktop_browser_close'),before),false,'background notification has no authority or lifecycle side effects')
 await serviceOpen('chat-b',bid,'https://b.example/')
 await page.getByRole('textbox',{name:'网址'}).waitFor()
 await page.waitForFunction(id=>activeTab===id,bid)
 await page.evaluate(()=>window.dispatchEvent(new CustomEvent('xharness:workspace-toggle')))
 await page.waitForFunction(()=>activeTab===null)
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_browser_close'||c.command==='desktop_browser_control_reply')),false,'collapsing is presentation only')
 const paused=await page.evaluate(()=>calls.length)
 await page.evaluate(()=>emitNative('xharness-browser-owned-page',{owner:'chat-a',tabId:'browser:'+'c'.repeat(32),closed:true}))
 await page.waitForTimeout(100)
 assert.equal(await page.evaluate(start=>calls.slice(start).some(c=>c.command==='desktop_browser_close'),paused),false,'native close notification is not replayed as a close command')
 await page.evaluate(()=>selectOwner('chat-a'))
 await page.getByRole('textbox',{name:'网址'}).waitFor()
 assert.equal(await page.getByRole('textbox',{name:'网址'}).inputValue(),'https://example.com/')
 const hid='headless:'+'1'.repeat(32)
 await page.evaluate(id=>{
   headless.set(id,{owner:'chat-a',mode:'agent',generation:0,url:'https://headless.example/'})
   emitNative('xharness-headless-page',{owner:'chat-a',tabId:id,url:'https://headless.example/',closed:false})
 },hid)
 assert.equal(await page.getByRole('textbox',{name:'网址'}).inputValue(),'https://example.com/','backend readiness cannot replace a selected preview')
 await page.getByRole('tab',{name:'headless.example',exact:true}).click()
 await page.getByRole('button',{name:'接管',exact:true}).click()
 await page.getByText('人工控制 · Agent 不可操作网页').waitFor()
 await page.getByRole('textbox',{name:'向网页输入文字'}).fill('测试')
 await page.getByRole('button',{name:'输入',exact:true}).click()
 assert.equal(await page.evaluate(()=>calls.filter(c=>c.command==='desktop_headless_manual'&&c.args.arguments.action==='text').at(-1)?.args.arguments.text),'测试')
 await page.evaluate(()=>window.dispatchEvent(new CustomEvent('xharness:workspace-toggle')))
 await page.waitForFunction(()=>!document.querySelector('.xhworkspace-item:not([hidden]) .xhbrowser-pane'))
 const readCount=await page.evaluate(()=>calls.filter(c=>c.command==='desktop_headless_preview').length)
 await page.waitForTimeout(1200)
 assert.equal(await page.evaluate(()=>calls.filter(c=>c.command==='desktop_headless_preview').length),readCount,'collapsed preview must stop screenshot requests')
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_headless_manual'&&c.args.arguments.action==='return')),false,'collapsing does not return control')
 await page.evaluate(()=>selectOwner('chat-b'))
 await page.waitForTimeout(1200)
 assert.equal(await page.evaluate(()=>calls.filter(c=>c.command==='desktop_headless_preview').length),readCount,'B cannot subscribe to A preview')
 await page.evaluate(()=>selectOwner('chat-a'))
 await page.evaluate(()=>window.dispatchEvent(new CustomEvent('xharness:workspace-toggle')))
 await page.getByRole('button',{name:'交还 Agent',exact:true}).click()
 await page.getByText('Agent 控制 · 只读预览').waitFor()
 assert.equal(await page.getByRole('textbox',{name:'向网页输入文字'}).count(),0)
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_headless_control')),false,'preview and takeover never create a page')
 await page.evaluate(()=>{window.failClose=true})
 await page.getByRole('button',{name:'关闭当前后台网页',exact:true}).click()
 await page.getByRole('alert').getByText('未确认浏览器已关闭，保留当前标签。').waitFor()
 assert.equal(await page.getByRole('tab',{name:'headless.example',exact:true}).count(),1)
 await page.evaluate(()=>{window.failClose=false})
 await page.getByRole('button',{name:'关闭当前后台网页',exact:true}).click()
 await page.getByRole('tab',{name:'headless.example',exact:true}).waitFor({state:'detached'})
 await page.evaluate(()=>root.unmount())
 assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_browser_close'||c.command==='desktop_browser_control_reply')),false,'renderer unmount cannot cancel service work')
 assert.deepEqual(errors,[])
 console.log(`${engine}: actual React chat switch/collapse/background-ready/close/idempotence and headless control passed (mocked native transport, not native acceptance)`)
}finally{await browser.close()}
