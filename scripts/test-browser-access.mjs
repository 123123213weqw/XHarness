import {installOwnedViewHtml} from './fixtures/owned-view-platform-browser.mjs'
import {ownedViewModuleTestInput} from './owned-view-module-test-input.mjs'
// Real React/DOM, mocked Tauri IPC. Not a native or model end-to-end score.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
const deps = resolve(process.env.UI_TEST_DEPS ?? '/Users/wangyue/codex-build/xharness-plugin-migration/ui-browser-deps')
const require = createRequire(resolve(deps, 'package.json'))
const { chromium, webkit } = require('playwright')
const engine = process.env.UI_TEST_BROWSER ?? 'chromium'
const browser = await ({ chromium, webkit })[engine].launch({ headless: true, ...(process.env.UI_TEST_EXECUTABLE?{executablePath:process.env.UI_TEST_EXECUTABLE}:{}) })
try {
  const errors = []
  async function fixture(native, lang = 'zh-CN', controlledClock = false) {
    const page = await browser.newPage({ viewport: { width: 900, height: 650 } })
    page.on('pageerror', error => {if(error.message!=='owned feature fixture: stop Host boot')errors.push(error.message)})
    if (controlledClock) await page.clock.install()
    await installOwnedViewHtml(page,process.env.UI_TEST_IMPL??'canonical',`<html lang="${lang}"><body style="margin:0"><div id="root" style="width:440px;height:620px"></div><div data-shell-overlay="true"></div></body></html>`)
    await page.addScriptTag({ content: `
      window.calls=[];window.listeners=[];window.active=null;window.tabs=new Set();window.lease=null;
      window.origin='https://example.com';window.holdGrant=false;window.badGrantReply=false;window.finishLoads=true;
      window.emit=(kind,value='')=>listeners.forEach(fn=>fn({payload:{tabId:'browser:1',kind,value}}));
      const receipt=owner=>({origin,grant:lease?.owner===owner&&lease.expires>Date.now()?{owner,allowActions:lease.actions,remainingMs:lease.expires-Date.now()}:null});
      window.__TAURI__=${native ? ` {core:{invoke:async(command,args)=>{
        calls.push({command,args});
        if(command==='desktop_browser_activate'){active=args.tabId;return tabs.has(active)}
        if(command==='desktop_browser_navigate'){
          tabs.add(args.tabId);active=args.tabId;origin=new URL(args.url).origin;
          if(finishLoads)setTimeout(()=>emit('loaded',args.url),0);
        }
        if(command==='desktop_browser_delegate'){
          if(holdGrant){window.grantStarted=true;await new Promise(resolve=>window.releaseGrant=resolve);holdGrant=false;}
          if(args.owner&&args.expectedOrigin!==origin){lease=null;throw Error('origin changed');}
          if(args.owner&&lease&&lease.owner!==args.owner)throw Error('browser belongs to another chat');lease=args.owner?{owner:args.owner,actions:args.allowActions,expires:Date.now()+600000}:null;
          return badGrantReply ? null : receipt(args.owner);
        }
      }},event:{listen:async(_name,fn)=>{listeners.push(fn);return()=>{listeners=listeners.filter(x=>x!==fn)}}}}` : 'null'};
      window.__ModuleLoader__={load:x=>window.registration=x};
    ` })
    await page.addScriptTag({ content: ownedViewModuleTestInput('@xlang/xharness-client-ui-browser') })
    await page.evaluate(() => {
      const plugin = registration.factory(id => staticModules[id] ?? (id === 'react' ? React : {}))
      plugin.apply({ get:()=>undefined,remote:{$on:()=>()=>{}},effect: fn => fn(), slots: { inject: (_name, fn) => fn(), register: () => {} } })
      window.root = ReactDOM.createRoot(document.getElementById('root'))
      function App() {
        const [item, setItem] = React.useState({ id: 'browser:1', kind: 'browser', entries: ['https://example.com/'], position: 0, title: 'example.com' })
        const [sessionId, setSession] = React.useState('session-parent')
        const [open, setOpen] = React.useState(true)
        window.setSession = setSession; window.setItem = setItem; window.setOpen = setOpen
        return React.createElement(plugin.BrowserPane, { item, sessionId, open, onUpdate: patch => setItem(value => ({ ...value, ...patch })), onClose: () => setOpen(false), onNewBrowser: () => {} })
      }
      root.render(React.createElement(App))
    })
    return page
  }
  async function noAccessUI(page) {
    assert.equal(await page.getByRole('radiogroup').count(), 0)
    assert.equal(await page.getByRole('button', { name: /Agent 访问|Agent access|授予|Grant|撤销|Revoke/ }).count(), 0)
    assert.equal(await page.locator('[class*="xhbrowser-access-"]').count(), 0)
    assert.equal(await page.getByText('session-parent', { exact: true }).count(), 0)
  }
  for (const lang of ['zh-CN', 'en']) {
    const page = await fixture(false, lang)
    await page.getByRole('textbox', { name: '网址' }).waitFor()
    await noAccessUI(page)
    assert.equal(await page.evaluate(() => calls.length), 0, 'ordinary Web must not simulate native access')
    assert.equal(await page.locator('iframe').count(), 0)
    await page.getByText(/网页版不能嵌入/).waitFor()
    await page.close()
  }
  const page = await fixture(true)
  await page.waitForFunction(() => lease?.owner === 'session-parent')
  await noAccessUI(page)
  assert.equal(await page.evaluate(() => lease.actions), true, 'model may choose observe or perform; Host approval still applies')
  const grants = () => page.evaluate(() => calls.filter(x => x.command === 'desktop_browser_delegate').length)
  const firstCount = await grants()
  await page.evaluate(() => window.dispatchEvent(new Event('resize')))
  await page.setViewportSize({ width: 940, height: 670 })
  await page.waitForTimeout(2100)
  assert.equal(await grants(), firstCount, 'no status polling or repeated grants on ordinary layout work')
  assert.equal(await page.evaluate(() => calls.some(x => x.command === 'desktop_browser_access')), false)
  assert.equal(await page.evaluate(() => calls.some(x => /inspect|perform/.test(x.command))), false, 'binding itself must not execute a model action')

  const lease=await page.evaluate(()=>window.lease)
  for(const boundary of ['download','modal','menu','collapse']){
    if(boundary==='download')await page.getByRole('button',{name:'下载记录',exact:true}).click()
    if(boundary==='collapse')await page.evaluate(()=>setOpen(false))
    if(boundary==='modal'||boundary==='menu')await page.evaluate(boundary=>{
      const node=document.createElement('div');node.id='test-overlay';node.role=boundary==='modal'?'dialog':'menu';
      if(boundary==='modal')node.setAttribute('aria-modal','true');else node.id='global-model-menu';
      node.style.cssText='position:fixed;left:300px;top:180px;width:200px;height:120px';document.querySelector('[data-shell-overlay]').append(node)
    },boundary)
    await page.waitForFunction(()=>active===null)
    assert.equal(await page.evaluate(()=>window.lease.owner),lease.owner,'occlusion cannot revoke owner')
    const hidden=await grants();await page.waitForTimeout(100);assert.equal(await grants(),hidden)
    if(boundary==='download')await page.getByRole('button',{name:'下载记录',exact:true}).click()
    if(boundary==='collapse')await page.evaluate(()=>setOpen(true))
    if(boundary==='modal'||boundary==='menu')await page.evaluate(()=>document.querySelector('#test-overlay,#global-model-menu').remove())
    await page.waitForFunction(()=>active==='browser:1')
  }
  // A stale/reused page passed to another chat cannot transfer its native lease.
  await page.evaluate(()=>setSession('foreign-owner'))
  await page.getByRole('alert').getByText('browser belongs to another chat',{exact:false}).waitFor()
  await page.waitForFunction(()=>active===null)
  assert.equal(await page.evaluate(()=>window.lease.owner),'session-parent')
  await page.evaluate(()=>setSession('session-parent'))
  await page.waitForFunction(()=>active==='browser:1')
  await page.evaluate(()=>root.unmount())
  await page.waitForFunction(()=>active===null)
  assert.equal(await page.evaluate(()=>window.lease.owner),'session-parent','unmount changes presentation only')
  assert.equal(await page.evaluate(()=>calls.some(c=>c.command==='desktop_browser_delegate'&&!c.args.owner)),false)
  assert.deepEqual(errors,[])
  await page.close()
  console.log(`${engine}: chat ownership survives presentation hide/overlays/unmount; foreign retarget rejected (mocked native transport)`)
}finally{await browser.close()}
