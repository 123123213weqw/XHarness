/** Regression for the old account entry. Real assembled UI; fixture Host only. */
import assert from 'node:assert/strict'
import { readFileSync, existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { resolve, sep, extname } from 'node:path'
import { fixtureUiRoot } from './fixtures/ui-fixture-build.mjs'

const engine = process.env.UI_TEST_BROWSER ?? 'chromium'
assert.ok(['chromium', 'webkit'].includes(engine))
const require = createRequire(resolve(process.env.UI_TEST_DEPS ?? 'scripts/ui-test-deps', 'package.json'))
const browser = await require('playwright')[engine].launch({ headless: true })
const base = fixtureUiRoot()
const types = { '.html': 'text/html', '.js': 'application/javascript', '.json': 'application/json', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.woff': 'font/woff', '.ttf': 'font/ttf', '.wasm': 'application/wasm' }
try {
  for (const lang of ['zh-CN', 'en-US']) for (const theme of ['light', 'dark']) {
    const page = await browser.newPage({ locale: lang, colorScheme: theme, viewport: { width: 1280, height: 820 } })
    const errors = []
    page.on('pageerror', error => errors.push(error.message))
    // Detect account side effects on boot even with a desktop bridge present.
    await page.addInitScript(() => {
      window.accountInvocations = []
      window.__TAURI__ = {
        core: { invoke: async command => { if (command.startsWith('desktop_account_')) window.accountInvocations.push(command); return null } },
        event: { listen: async name => { if (name === 'xharness-account-ready') window.accountInvocations.push(name); return () => {} } },
      }
    })
    await page.route('**/*', route => {
      const url = new URL(route.request().url())
      const file = resolve(base, '.' + (url.pathname === '/' ? '/index.html' : decodeURIComponent(url.pathname)))
      if (url.hostname !== '127.0.0.1' || !file.startsWith(base + sep) || !existsSync(file)) return route.abort('blockedbyclient')
      return route.fulfill({ body: readFileSync(file), contentType: types[extname(file)] ?? 'application/octet-stream' })
    })
    await page.goto('http://127.0.0.1:39187/?fixture=1')
    const zh = lang.startsWith('zh')
    const title = zh ? '设置' : 'Settings'
    const trigger = page.getByRole('button', { name: title, exact: true })
    const dialog = page.getByRole('dialog', { name: title, exact: true })
    const close = dialog.getByRole('button', { name: zh ? '关闭' : 'Close', exact: true })
    await trigger.waitFor()
    assert.equal(await trigger.count(), 1)
    assert.equal(await trigger.getAttribute('aria-haspopup'), 'dialog')
    assert.equal(await trigger.locator('svg').count(), 1)
    assert.equal(await trigger.innerText(), title)
    assert.equal(await page.locator('[data-xh-account-trigger],[data-xharness-assistant-nav]').count(), 0)
    await trigger.click()
    await dialog.waitFor()
    assert.equal(await dialog.locator('nav button[aria-current="true"]').innerText(), zh ? '通用设置' : 'General')
    assert.equal(await dialog.getByRole('button', { name: /Account|账号|账户/ }).count(), 0)
    assert.equal(await page.getByRole('menu').count(), 0)
    await dialog.getByRole('button', { name: zh ? '使用档案' : 'Profile', exact: true }).click()
    await dialog.locator('.xhp-root').waitFor()
    await close.click()
    assert.equal(await trigger.evaluate(el => el === document.activeElement), true, 'pointer close returns focus to Settings')
    await trigger.press('Enter')
    await dialog.waitFor()
    await page.keyboard.press('Escape')
    await dialog.waitFor({ state: 'hidden' })
    await page.waitForFunction(() => document.activeElement?.hasAttribute('data-xh-settings-trigger'))

    await page.getByRole('button', { name: zh ? '收起侧边栏' : 'Collapse sidebar', exact: true }).click()
    await page.setViewportSize({ width: 426, height: 664 })
    await page.waitForFunction(() => {
      const entry = document.querySelector('[data-xh-settings-trigger]')
      const sidebar = entry?.closest('.U910La_root')
      return innerWidth === 426 && sidebar?.classList.contains('U910La_collapsed')
        && !sidebar.classList.contains('U910La_fading')
    })
    assert.equal(await trigger.evaluate(el => el.querySelector('span').getBoundingClientRect().width <= 1), true, 'rail label is visually hidden')
    await trigger.click()
    await dialog.getByRole('button', { name: zh ? '模型' : 'Models', exact: true }).click()
    await dialog.getByRole('button', { name: zh ? '添加自定义提供方' : 'Add a custom provider', exact: true }).waitFor()
    await close.click()
    assert.deepEqual(await page.evaluate(() => window.accountInvocations), [], 'no desktop login or callback subscription')
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log(`${engine}: bilingual light/dark Settings, focus restoration, compact rail, local usage and model setup without an account passed`)
} finally { await browser.close() }
