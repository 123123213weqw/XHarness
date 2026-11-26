// Regression for retiring the account model menu. Actual source picker and primitives, fixture RPC only.
import assert from 'node:assert/strict'
import {createRequire} from 'node:module'
import {resolve} from 'node:path'
import {createModelPreview} from './preview-model-menu.mjs'

const require = createRequire(resolve(process.env.UI_TEST_DEPS ?? 'scripts/ui-test-deps', 'package.json'))
const engine = process.env.UI_TEST_BROWSER ?? 'chromium'
const browser = await require('playwright')[engine].launch({headless:true})
const server = createModelPreview()
await new Promise(done => server.listen(0,'127.0.0.1',done))
const origin = `http://127.0.0.1:${server.address().port}`
try {
  for (const lang of ['zh','en']) for (const theme of ['dark','light']) {
    const zh = lang === 'zh'
    const page = await browser.newPage({viewport:{width:900,height:620}})
    const errors = []
    page.on('pageerror',e=>{if (!e.message.includes('Host boot intentionally disabled')) errors.push(e.message)})
    page.setDefaultTimeout(10000)
    await page.goto(`${origin}/?lang=${lang}&theme=${theme}`)
    const trigger = page.locator('#fixture-picker button[data-xh-model-control=model]')
    const open = async () => {
      await trigger.click()
      await page.keyboard.press('ArrowDown')
      const row = page.getByRole('menuitem',{name:zh?/^模型 /:/^Model /})
      assert.equal(await row.evaluate(el=>el===document.activeElement),true)
      await page.keyboard.press('Enter')
      await page.getByRole('menuitem',{name:zh?'管理模型':'Manage models',exact:true}).waitFor()
    }
    await open()
    assert.deepEqual(await page.getByRole('menuitemradio').allTextContents(), ['Gateway GLM','My GLM','Local model'])
    assert.equal(await page.getByRole('menuitem',{name:/^(Custom|自定义)$/}).count(),0)
    assert.equal(await page.getByText(zh?'账号服务':'Account service',{exact:true}).count(),0)
    assert.equal(await page.getByRole('group',{name:'XHarness',exact:true}).count(),1,'a BYOK display name is not an account marker')
    assert.equal(await page.evaluate(()=>fixture.calls.length),0,'opening or loading may not change a retired selection')
    await page.getByRole('menuitemradio',{name:'Gateway GLM',exact:true}).click()
    await page.waitForFunction(()=>fixture.saved.provider==='gateway')
    assert.equal(await trigger.getAttribute('aria-expanded'),'false')
    assert.equal(await trigger.evaluate(el=>el===document.activeElement),true)
    await open()
    await page.getByRole('menuitemradio',{name:'My GLM',exact:true}).click()
    await page.waitForFunction(()=>fixture.saved.provider==='bigmodel')
    assert.equal(await page.evaluate(()=>fixture.saved.model),'glm','same model ID selects the explicit provider')
    await open()
    await page.getByRole('menuitem',{name:zh?'管理模型':'Manage models',exact:true}).click()
    await page.getByTestId('models-settings').waitFor()
    await page.getByRole('button',{name:zh?'关闭':'Close',exact:true}).click()
    assert.equal(await trigger.evaluate(el=>el===document.activeElement),true,'Settings close restores composer focus')
    await page.evaluate(async()=>{fixture.empty=true;await fixture.directory.load()})
    await open()
    await page.getByText(zh?'没有可用的模型。':'No models available.',{exact:true}).waitFor()
    await page.keyboard.press('ArrowLeft')
    await page.getByRole('menuitem',{name:zh?/^模型 /:/^Model /}).waitFor()
    await page.keyboard.press('Escape')
    await page.evaluate(async()=>{fixture.empty=false;await fixture.directory.load();fixture.offline=true})
    await open()
    assert.equal(await page.getByText(/offline/).count(),0,'cached menu remains usable without network')
    for (const width of [900,520,320]) {
      await page.setViewportSize({width,height:620})
      await page.waitForFunction(() => {
        const menu = document.querySelector('#fixture-picker [role="menu"]')
        const box = menu?.getBoundingClientRect()
        return box && box.x >= 0 && box.y >= 0 && box.right <= innerWidth && box.bottom <= innerHeight
      })
    }
    assert.deepEqual(errors,[])
    await page.close()
  }
  console.log(`${engine}: 4 bilingual/theme scenarios; flat providers, explicit replacement of retired model, keyboard/focus, cached/empty and narrow menu passed`)
} finally { await browser.close(); await new Promise(done => server.close(done)) }
