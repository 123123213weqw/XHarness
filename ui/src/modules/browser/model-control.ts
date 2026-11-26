import type { TauriBridge, NativeUnlisten } from '../shared/tauri'
import { objectValue } from '../shared/runtime-types'

/** Service-completed pages update their owning workspace without changing
 * selected chat, creating pages, issuing grants or sending UI acknowledgements. */
export interface OwnedBrowserPage {owner:string;tabId:string;backend:"webview"|"browser";closed:boolean;url?:string}
export function ownedBrowserPage(raw:unknown,backend:OwnedBrowserPage["backend"]):OwnedBrowserPage|undefined {
 const value=objectValue(raw)
 if(typeof value.owner!=='string'||!value.owner||value.owner.length>128||typeof value.tabId!=='string'||!(backend==="browser"?/^headless:[0-9a-f]{32}$/:/^browser:[A-Za-z0-9_-]{1,56}$/).test(value.tabId)||typeof value.closed!=='boolean')return
 if(value.closed)return{owner:value.owner,tabId:value.tabId,backend,closed:true}
 if(typeof value.url!=='string'||value.url.length>4096)return
 try{const url=new URL(value.url);if(!['http:','https:'].includes(url.protocol)||url.username||url.password)return;return{owner:value.owner,tabId:value.tabId,backend,closed:false,url:url.href}}catch{return}
}
export function listenOwnedBrowser(native:TauriBridge,accept:(page:OwnedBrowserPage)=>void):()=>void {
 let disposed=false
 const stops:NativeUnlisten[]=[]
 const subscriptions=[["xharness-browser-owned-page","webview"],["xharness-headless-page","browser"]] as const
 for(const [event,backend] of subscriptions){
   void native.event.listen(event,ev=>{if(disposed)return;const page=ownedBrowserPage(ev.payload,backend);if(page)accept(page)})
     .then(unlisten=>{if(disposed)unlisten();else stops.push(unlisten)})
     .catch(()=>{if(!disposed)console.error("Browser presentation subscription failed")})
 }
 return()=>{disposed=true;stops.forEach(stop=>stop())}
}
