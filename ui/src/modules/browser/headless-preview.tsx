import * as React from 'react'
import type {BrowserPaneProps} from './index'
import {objectValue} from '../shared/runtime-types'

const {createElement:h,useEffect,useRef,useState}=React
let previewReadInFlight=false
type Driver='agent'|'human'|'switching'
interface Control {mode:Driver;generation:string;uncertain:boolean}
interface Preview {image:string;manualFrame?:string}
const MANUAL='desktop_headless_manual'

function controlValue(raw:unknown):Control|undefined {
  const value=objectValue(raw)
  if(value.mode!=='agent'&&value.mode!=='human'&&value.mode!=='switching')return
  if(typeof value.generation!=='string'||!/^(0|[1-9][0-9]{0,19})$/.test(value.generation))return
  return{mode:value.mode,generation:value.generation,uncertain:value.prior_effect_uncertain===true}
}
export function previewPoint(rect:{left:number;top:number;width:number;height:number},x:number,y:number):{x:number;y:number}|undefined {
  const scale=Math.min(rect.width/1280,rect.height/720)
  if(!Number.isFinite(scale)||scale<=0)return
  const px=(x-rect.left-(rect.width-1280*scale)/2)/scale
  const py=(y-rect.top-(rect.height-720*scale)/2)/scale
  if(px<0||px>=1280||py<0||py>=720||!Number.isFinite(px)||!Number.isFinite(py))return
  return{x:px,y:py}
}

/** The sidebar is a subscriber, not the owner. Closing it does not release the
 * human seat, recreate the page or consume an Agent screenshot credential. */
export function HeadlessPreview(props:BrowserPaneProps) {
  const {item,sessionId,open}=props
  const current=useRef(props);current.current=props
  const initial=item.entries[item.position]??''
  const [draft,setDraft]=useState(initial)
  const [preview,setPreview]=useState<Preview>()
  const [control,setControl]=useState<Control>()
  const [transportError,setTransportError]=useState('')
  const [actionError,setActionError]=useState('')
  const [working,setWorking]=useState(false)
  const [text,setText]=useState('')
  const writing=useRef(false),revision=useRef(0),mounted=useRef(false)
  useEffect(()=>{mounted.current=true;return()=>{mounted.current=false}},[])
  useEffect(()=>setDraft(initial),[initial])
  useEffect(()=>{
    setPreview(undefined);setControl(undefined)
    if(!open||!sessionId||!item.id.startsWith('headless:'))return
    let disposed=false,timer:ReturnType<typeof setTimeout>|undefined,running=false
    const visible=()=>!disposed&&!document.hidden&&current.current.open===true
    const schedule=(poll:()=>void)=>{if(visible())timer=setTimeout(poll,1000)}
    const poll=async()=>{
      if(!visible()||running)return
      if(previewReadInFlight||writing.current){schedule(poll);return}
      running=true;previewReadInFlight=true
      const started=revision.current
      try{
        const native=window.__TAURI__;if(!native)throw Error('后台浏览器需要桌面客户端')
        const raw=objectValue(await native.core.invoke('desktop_headless_preview',{owner:sessionId,tabId:item.id}))
        if(!visible()||started!==revision.current)return
        const next=controlValue(raw.control)
        setControl(next)
        if(raw.busy===true){
          if(next?.mode!=='human')setPreview(value=>value?.manualFrame?{image:value.image}:value)
          return
        }
        if(raw.tabId!==item.id||raw.mime!=='image/jpeg'||raw.width!==640||raw.height!==360||typeof raw.image!=='string'||raw.image.length>2*1024*1024)throw Error('无效的浏览器预览')
        const image='data:image/jpeg;base64,'+raw.image
        setPreview(typeof raw.manualFrame==='string'&&/^[a-f0-9]{32}$/.test(raw.manualFrame)&&next?.mode==='human'?{image,manualFrame:raw.manualFrame}:{image})
        setTransportError('')
        if(typeof raw.url==='string'&&raw.url.length<=4096&&raw.url!==current.current.item.entries[current.current.item.position]){
          try{const url=new URL(raw.url);if(['http:','https:'].includes(url.protocol)&&!url.username&&!url.password){const entries=[...current.current.item.entries.slice(-49),url.href];current.current.onUpdate({entries,position:entries.length-1,title:typeof raw.title==='string'?raw.title.slice(0,160):url.hostname})}}catch{/* Untrusted guest address is not executable UI. */}
        }
      }catch(problem){if(visible()&&started===revision.current){setTransportError(String(problem));setControl(undefined);setPreview(undefined)}}
      finally{running=false;previewReadInFlight=false;schedule(poll)}
    }
    const visibility=()=>{
      clearTimeout(timer)
      ++revision.current
      if(document.hidden){setPreview(undefined);setControl(undefined)}
      else void poll()
    }
    document.addEventListener('visibilitychange',visibility)
    void poll()
    return()=>{disposed=true;clearTimeout(timer);document.removeEventListener('visibilitychange',visibility)}
  },[item.id,sessionId,open])

  const invoke=async(action:string,args:Record<string,unknown>={}):Promise<boolean>=>{
    const native=window.__TAURI__
    if(!native||!open||!sessionId||!control||writing.current||document.hidden)return false
    writing.current=true;++revision.current
    setWorking(true);setActionError('')
    setPreview(value=>value?{image:value.image}:undefined)
    try{
      const raw=objectValue(await native.core.invoke(MANUAL,{owner:sessionId,tabId:item.id,arguments:{action,generation:control.generation,...args}}))
      if(!mounted.current)return false
      if(action==='takeover'||action==='return'){
        const next=controlValue(raw)
        if(!next)throw Error('控制权状态未确认，请等待刷新')
        setControl(next);return true
      }
      if(raw.ok!==true)throw Error(typeof raw.message==='string'?raw.message:'操作未确认，请先检查页面')
      return true
    }catch(problem){if(mounted.current)setActionError(String(problem));return false}
    finally{writing.current=false;++revision.current;if(mounted.current)setWorking(false)}
  }
  const navigate=async(event:React.FormEvent)=>{
    event.preventDefault()
    if(control?.mode!=='human')return
    const value=draft.trim();if(!value)return
    let url:URL
    try{url=new URL(/^[a-z][a-z\d+.-]*:\/\//i.test(value)?value:`${/^(localhost|127\.0\.0\.1|\[::1\])(?::|\/|$)/i.test(value)?'http':'https'}://${value}`);if(!['http:','https:'].includes(url.protocol)||url.username||url.password)throw Error('invalid')}
    catch{setActionError('请输入有效的 http/https 网址');return}
    await invoke('navigate',{url:url.href})
  }
  const human=control?.mode==='human'
  const frame=human&&!working?preview?.manualFrame:undefined
  const click=(event:React.MouseEvent<HTMLImageElement>)=>{
    if(!frame)return
    const point=previewPoint(event.currentTarget.getBoundingClientRect(),event.clientX,event.clientY)
    if(point)void invoke('click',{frame_id:frame,...point})
  }
  const sendText=async(event:React.FormEvent)=>{
    event.preventDefault();if(!frame||!text)return
    if(await invoke('text',{frame_id:frame,text}))setText('')
  }
  const status=working||control?.mode==='switching'?'正在切换或操作…':human?'人工控制 · Agent 不可操作网页':control?.mode==='agent'?'Agent 控制 · 只读预览':'后台浏览器 · 等待状态'
  return h('section',{className:'xhbrowser-pane','aria-label':'后台浏览器预览','data-headless-preview':true},
    h('form',{className:'xhbrowser-toolbar',onSubmit:navigate},
      h('input',{className:'xhbrowser-headless-address','aria-label':'后台浏览器网址',value:draft,readOnly:!human,onChange:(e:React.ChangeEvent<HTMLInputElement>)=>setDraft(e.currentTarget.value),placeholder:'输入网址'}),
      human&&h('button',{type:'submit',disabled:working},'打开'),
      h('button',{type:'button',onClick:()=>current.current.onClose(),'aria-label':'关闭当前后台网页'},'×')),
    (actionError||transportError)&&h('p',{className:'xhbrowser-error',role:'alert'},actionError||transportError),
    control?.uncertain&&h('p',{className:'xhbrowser-headless-note',role:'status'},'上一次操作结果不确定；请先检查页面，不要重复提交。'),
    h('div',{className:'xhbrowser-headless-canvas'},open&&preview?h('img',{
      src:preview.image,alt:'当前后台浏览器画面',draggable:false,onClick:click,'data-manual-click':Boolean(frame)||undefined,
    }):h('p',null,'等待页面预览…')),
    human&&h('form',{className:'xhbrowser-headless-input',onSubmit:sendText},
      h('input',{'aria-label':'向网页输入文字',placeholder:'先点击网页中的输入框',value:text,maxLength:1000,onChange:(e:React.ChangeEvent<HTMLInputElement>)=>setText(e.currentTarget.value)}),
      h('button',{type:'submit',disabled:!frame||!text},'输入'),
      h('button',{type:'button',disabled:!frame,onClick:()=>{if(frame)void invoke('key',{frame_id:frame,key:'Enter'})}},'Enter')),
    h('footer',{className:'xhbrowser-headless-footer'},
      h('span',{role:'status'},status),
      control&&(control.mode==='human'||control.mode==='agent')&&h('button',{type:'button',disabled:working,onClick:()=>{void invoke(human?'return':'takeover')}},human?'交还 Agent':'接管'),
      h('span',null,human?'收起后仍由你控制':'收起不停止任务')))
}
