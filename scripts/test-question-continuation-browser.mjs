// Actual shipped React, primitives, workspace owner and product flow; only the
// host services/slot harness are fixtures. No running App or user data is used.
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import {installOwnedViewPlatform} from './fixtures/owned-view-platform-browser.mjs'
import {exposeModuleUnit} from './fixtures/module-unit-scope.mjs'
import {verifyArtifact} from './fixtures/shipped-source-values.mjs'
const root = fileURLToPath(new URL('../', import.meta.url))
const implementation=process.env.UI_TEST_IMPL??'source';assert.ok(['source','native','legacy'].includes(implementation))
const dist = resolve(process.env.UI_TEST_DIST ?? resolve(root, implementation==='legacy'?'ui/reference/master-a613970':'ui/dist'))
const require = createRequire(resolve(process.env.UI_TEST_DEPS ?? '/tmp/ui-tests', 'package.json'))
const engines = require('playwright')
const engine = process.env.UI_TEST_BROWSER ?? 'chromium'
const browser = await engines[engine].launch({ headless: true,
  ...(process.env.UI_TEST_EXECUTABLE ? { executablePath: process.env.UI_TEST_EXECUTABLE } : {}) })
try {
  const page = await browser.newPage({ viewport: { width: 960, height: 720 } })
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  page.setDefaultTimeout(8000)
  await installOwnedViewPlatform(page,implementation==='legacy'?'legacy':'source')
  await page.evaluate(() => { document.getElementById('root').replaceChildren(); window.registrations = {}; window.__ModuleLoader__ = { load: reg => { registrations[reg.id] = reg } } })
  const plugins = readdirSync(resolve(dist,'plugins/@xharness'));
  for (const name of plugins) {
    const file = resolve(dist,'plugins/@xharness',name,'client.js');
    let source=readFileSync(file,'utf8');
    if(name==='dsh-client-ui-user-questions') {
      if(implementation!=='legacy')assert.equal(source,verifyArtifact('@xharness/dsh-client-ui-user-questions'),'actual Question artifact is source-fresh')
      source=exposeModuleUnit(source,'user-questions','QuestionComposer','QuestionComposer')
    }
    await page.addScriptTag({content:source});
  }
  await page.evaluate(() => {
    const cache={};function load(id){if(staticModules[id])return staticModules[id];const name=id.endsWith('/client')?id.slice(0,-7):id;if(cache[name])return cache[name];return cache[name]=registrations[name].factory(load)};
    const React=staticModules.react,DOM=staticModules['react-dom'];
    const module=load('@xharness/dsh-client-ui-user-questions/client');
    const root=DOM.createRoot(document.getElementById('root'));window.answers=[];
    window.renderQuestion=(deferred=false,key='q:test',questions=[{id:'target',header:'选择',question:'Choose target?',options:[],multiSelect:false}])=>{
      const wait={key,sessionId:'test',payload:{deferred,questions},respond:async answer=>{answers.push(answer);if(window.answerBlock)await window.answerBlock;if(window.answerFailure)throw Error(window.answerFailure);return window.answerReceipt??{accepted:true}}};
      DOM.flushSync(()=>root.render(React.createElement('div',{'data-composer-seat':''},
        React.createElement('div',{'data-slot':'conversation.composer'},
          React.createElement('div',{'data-chain-overlay-fallback':'conversation.composer',style:{display:'none'}},
            React.createElement('div',{'data-normal-composer':''},React.createElement('textarea',{'aria-label':'Message the agent'}))),
          key==='approval' ? React.createElement('div',{'data-approval':''},'Approval required') :
          React.createElement(module.QuestionComposer,{matched:wait,t:key=>key})))));
    };renderQuestion();
  });
  const input=page.locator('[data-question-key] textarea');
  const composer=page.getByRole('textbox',{name:'Message the agent',exact:true});
  assert.equal(await composer.isVisible(),false,'blocking question retains takeover');
  // The untouched textarea is auto-focused. That alone must not count as
  // user activity or the idle card would never collapse.
  assert.equal(await input.evaluate(node=>document.activeElement===node),true);
  if (implementation==='legacy') await input.fill('my partial answer');
  await page.evaluate(()=>renderQuestion(true));
  await page.locator('[data-question-deferred="true"]').waitFor();
  await input.waitFor({state:'detached'});
  assert.equal(await input.count(),0,'idle timeout automatically folds the question body');
  assert.equal(await composer.isVisible(),true,'deferred question releases normal composer');
  await composer.fill('independent message draft');
  await page.getByRole('button',{name:'展开待回答问题',exact:true}).click();
  if (implementation!=='legacy') await input.fill('my partial answer');
  assert.equal(await input.inputValue(),'my partial answer');
  assert.equal(await composer.inputValue(),'independent message draft');
  assert.equal(await page.evaluate(()=>answers.length),0,'timeout is not an answer');
  await page.getByRole('status').getByText(/等待回答/).waitFor();
  await page.evaluate(()=>renderQuestion(true));
  assert.equal(await page.locator('[data-question-key]').count(),1,'replayed notification does not duplicate question');
  assert.equal(await input.inputValue(),'my partial answer');
  assert.equal(await input.isVisible(),true,'duplicate deferred frame must not fold manually reopened card');
  await page.getByRole('button',{name:'nav.minimize',exact:true}).click();
  assert.equal(await composer.isVisible(),true);
  await page.setViewportSize({width:390,height:720});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=390),true,'narrow viewport must not overflow');
  await page.getByRole('button',{name:'展开待回答问题',exact:true}).click();
  assert.equal(await input.inputValue(),'my partial answer');
  await page.evaluate(()=>renderQuestion(true,'q:restored'));
  assert.equal(await input.count(),0,'restored deferred question starts folded');
  await page.evaluate(()=>renderQuestion(false,'q:another'));
  assert.equal(await input.inputValue(),'','different question must not inherit another draft');
  assert.equal(await composer.isVisible(),false,'next blocking question takes over');
  await page.evaluate(()=>renderQuestion(true,'approval'));
  assert.equal(await composer.isVisible(),false,'approval winner must never be unblocked');
  assert.equal(await page.getByRole('status').filter({hasText:'等待回答'}).count(),0);

  if (implementation!=='legacy') {
    const card=page.locator('[data-question-key]');
    // Inject the exact same-key deferred notification emitted by Host at 60s,
    // precisely while the gesture is in progress; do not wait for a wall clock.
    const stillOpen=async () => {
      // Let the passive effect and its resulting render settle before checking.
      await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
      await page.getByRole('status').getByText(/可继续不依赖答案的工作/).waitFor();
      assert.equal(await card.getAttribute('data-question-minimized'),'false');
      assert.equal(await composer.isVisible(),true,'keeping the card open must not block the ordinary composer');
    };
    await page.evaluate(()=>renderQuestion(false,'q:typing-at-deadline'));
    const draft='我正在写第 60 秒的回答';
    await page.keyboard.insertText(draft);
    await input.evaluate(node=>{
      node.setSelectionRange(2,5);
      window.beforeDeadlineInput=node;
    });
    await page.evaluate(()=>renderQuestion(true,'q:typing-at-deadline'));
    await stillOpen();
    assert.equal(await input.inputValue(),draft);
    assert.deepEqual(await input.evaluate(node=>({
      sameNode:node===window.beforeDeadlineInput,
      active:document.activeElement===node,
      caret:[node.selectionStart,node.selectionEnd],
    })),{sameNode:true,active:true,caret:[2,5]},'deferred event must not remount or defocus the editor');
    await page.evaluate(()=>renderQuestion(true,'q:typing-at-deadline'));
    assert.equal(await input.evaluate(node=>document.activeElement===node),true,'replaying deferral must not steal focus');
    await page.getByRole('button',{name:'nav.minimize',exact:true}).click();
    await input.waitFor({state:'detached'});
    await page.getByRole('button',{name:'展开待回答问题',exact:true}).click();
    assert.equal(await input.inputValue(),draft,'manual folding still preserves the draft');

    await page.evaluate(()=>renderQuestion(false,'q:ime-at-deadline'));
    await input.dispatchEvent('compositionstart',{data:'',bubbles:true});
    assert.equal(await input.inputValue(),'','pre-edit need not have a committed value');
    await page.evaluate(()=>renderQuestion(true,'q:ime-at-deadline'));
    await stillOpen();
    assert.equal(await input.evaluate(node=>document.activeElement===node),true,'IME pre-edit must keep focus before any text change');
    await input.dispatchEvent('compositionend',{data:'中文',bubbles:true});
    await page.keyboard.insertText('中文');
    assert.equal(await input.inputValue(),'中文','the focused field can finish editing after deferral');

    await page.evaluate(()=>{
      window.selectionQuestions=[{id:'pick',header:'选择',question:'Pick?',options:[{label:'One'},{label:'Two'}]}];
      renderQuestion(false,'q:pick-at-deadline',window.selectionQuestions);
    });
    const option=page.getByRole('radio',{name:'Two',exact:true});
    await option.hover();
    const optionBefore=await option.boundingBox();
    await page.mouse.down();
    await page.evaluate(()=>renderQuestion(true,'q:pick-at-deadline',window.selectionQuestions));
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    assert.equal(await card.getAttribute('data-question-deferred'),'false','visual deferral waits for the pointer release');
    assert.deepEqual(await option.boundingBox(),optionBefore,'the pressed option must not move under the pointer');
    await page.mouse.up();
    await stillOpen();
    assert.equal(await option.getAttribute('aria-checked'),'true','a press started before timeout still completes its click');

    await page.evaluate(()=>renderQuestion(false,'q:release-outside',window.selectionQuestions));
    await option.hover();
    await page.mouse.down();
    await page.evaluate(()=>renderQuestion(true,'q:release-outside',window.selectionQuestions));
    await page.mouse.move(0,0);
    await page.mouse.up();
    await stillOpen();
    assert.equal(await option.getAttribute('aria-checked'),'false','release outside the card clears the hold without choosing an answer');

    await page.evaluate(()=>renderQuestion(false,'q:pointer-cancelled',window.selectionQuestions));
    await option.hover();
    await option.evaluate(node=>node.addEventListener('pointerdown',event=>{window.questionPointerId=event.pointerId},{once:true}));
    await page.mouse.down();
    await page.evaluate(()=>renderQuestion(true,'q:pointer-cancelled',window.selectionQuestions));
    await option.dispatchEvent('pointercancel',{pointerId:await page.evaluate(()=>questionPointerId),bubbles:true});
    await stillOpen();
    await page.mouse.move(0,0);
    await page.mouse.up();
    assert.equal(await option.getAttribute('aria-checked'),'false','cancelled pointer cannot keep layout on hold or select anything');

    await page.evaluate(()=>renderQuestion(false,'q:keyboard-at-deadline',window.selectionQuestions));
    await option.focus();
    await option.press('Space');
    await page.evaluate(()=>renderQuestion(true,'q:keyboard-at-deadline',window.selectionQuestions));
    await stillOpen();
    assert.equal(await option.getAttribute('aria-checked'),'true');
    assert.equal(await option.evaluate(node=>document.activeElement===node),true,'keyboard selection should retain focus');

    await page.evaluate(()=>{
      window.answers=[];
      window.answerBlock=new Promise(resolve=>{window.releaseAnswer=resolve});
      renderQuestion(false,'q:submit-at-deadline');
    });
    await input.fill('submission draft');
    await page.getByRole('button',{name:'submit',exact:true}).click();
    assert.equal(await page.evaluate(()=>answers.length),1,'submit has started but has no ACK yet');
    assert.equal(await page.getByRole('button',{name:'submitting',exact:true}).isDisabled(),true);
    // Browsers may blur a newly disabled input/button on a later paint.
    // Test deferral with a stable usable focus target within the same card,
    // rather than comparing an element the submission itself just disabled.
    const stableSubmitFocus=await input.evaluate(node=>{
      const card=node.closest('section');
      card.tabIndex=-1;
      card.focus();
      window.beforeSubmitDeferralInput=node;
      window.beforeSubmitDeferralFocus=card;
      return document.activeElement===card;
    });
    assert.equal(stableSubmitFocus,true,'focus is established before deferral');
    await page.evaluate(()=>renderQuestion(true,'q:submit-at-deadline'));
    await stillOpen();
    assert.equal(await input.inputValue(),'submission draft');
    assert.equal(await input.evaluate(node=>node===window.beforeSubmitDeferralInput &&
      document.activeElement===window.beforeSubmitDeferralFocus),true,'deferral does not add focus changes during submission');
    assert.equal(await page.getByRole('button',{name:'submitting',exact:true}).isDisabled(),true);
    await page.evaluate(()=>{
      window.answerFailure='fixture timeout during submit';
      window.releaseAnswer();
      window.answerBlock=undefined;
    });
    await page.getByRole('status').getByText('fixture timeout during submit',{exact:true}).waitFor();
    assert.equal(await input.inputValue(),'submission draft','submission failure leaves the preserved draft available to retry');
    assert.equal(await page.getByRole('button',{name:'submit',exact:true}).isEnabled(),true);
    assert.equal(await page.evaluate(()=>answers.length),1);
    await page.evaluate(()=>{window.answerFailure=undefined});
  }

  await page.evaluate(()=>{answers=[];renderQuestion(false,'q:multi',[{id:'a',question:'Multiple?',multiSelect:true,options:[{label:'Fast (Recommended)'},{label:'Safe'}]},{id:'b',question:'Free?',options:[]}])});
  await page.getByRole('checkbox',{name:'Fast',exact:true}).click();
  await page.getByRole('checkbox',{name:'Safe',exact:true}).click();
  await page.locator('[data-question-key] input').fill(' plus custom ');
  await page.getByRole('button',{name:'action.next',exact:true}).click();
  await page.locator('[data-question-key] textarea').fill('second answer');
  await page.locator('[data-question-key] textarea').evaluate(node=>node.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',keyCode:229,isComposing:true,bubbles:true})));
  assert.equal(await page.evaluate(()=>answers.length),0,'IME Enter cannot submit');
  await page.getByRole('button',{name:'submit',exact:true}).click();
  assert.deepEqual(await page.evaluate(()=>answers),[{ok:true,value:{sessionId:'test',answer:{answers:[{id:'a',selected:['Fast (Recommended)','Safe'],custom:'plus custom'},{id:'b',selected:[],custom:'second answer'}]}}}]);
  assert.equal(await page.getByRole('button',{name:'submitting',exact:true}).isDisabled(),true,'accepted answer remains one-shot until host resolves');
  await page.evaluate(()=>{answers=[];answerReceipt={accepted:false,reason:'stale'};renderQuestion(false,'q:retry')});
  await page.locator('[data-question-key] textarea').fill('preserve retry');
  await page.getByRole('button',{name:'submit',exact:true}).click();
  await page.getByRole('status').getByText('question response rejected: stale',{exact:true}).waitFor();
  assert.equal(await page.locator('[data-question-key] textarea').inputValue(),'preserve retry');
  await page.evaluate(()=>{answerReceipt=undefined;answerFailure='fixture offline'});
  await page.getByRole('button',{name:'submit',exact:true}).click();
  await page.getByRole('status').getByText('fixture offline',{exact:true}).waitFor();
  assert.equal(await page.getByRole('button',{name:'submit',exact:true}).isEnabled(),true,'transport failure re-arms answer');
  await page.evaluate(()=>{answerFailure=undefined;answers=[];renderQuestion(false,'q:skip')});
  await page.getByRole('button',{name:'action.skip',exact:true}).click();
  assert.deepEqual(await page.evaluate(()=>answers[0].value.answer),{answers:[{id:'target',selected:[]}]});
  await page.evaluate(()=>{answers=[];renderQuestion(false,'q:plan',[{id:'plan',question:'Approve?',detail:'# Reviewed plan',intent:{kind:'plan-review',approve:'Go'},options:[{label:'Go'},{label:'Stop'}]}])});
  await page.getByRole('button',{name:'plan.approve',exact:true}).click();
  assert.deepEqual(await page.evaluate(()=>answers[0].value.answer),{answers:[{id:'plan',selected:['Go']}]});
  await page.evaluate(()=>{answers=[];renderQuestion(false,'q:discuss',[{id:'plan',question:'Approve?',detail:'# Reviewed plan',intent:{kind:'plan-review',approve:'Go'},options:[{label:'Go'}]}])});
  await page.getByRole('button',{name:'plan.discuss',exact:true}).click();
  assert.equal(await page.evaluate(()=>answers[0].error.code),'cancelled');
  assert.deepEqual(errors.filter(e=>e!=='owned feature fixture: stop Host boot'),[]);
  console.log(engine+'/'+implementation+': question deferred banner, draft preserved, no implicit answer, repeated frame, session isolation, automatic fold, restored fold, normal composer, approval isolation, narrow layout, multiselect/custom, IME, editing/selection/submit at deferral (source builds), rejected/failed retry, one-shot, skip and plan decision passed');
} finally {await browser.close()}
