import {useEffect,useRef,useState,type ClipboardEvent,type DragEvent} from 'react';
import {ArrowLeft,ChevronRight,ImagePlus,MessageCircle,Pencil,Plus,Send,Square,Trash2,User,X,ExternalLink} from 'lucide-react';
import type {Locale} from './model';
import {categoryKeys} from './model';
import {t} from './i18n';
import {aiSettingsText} from './ai-settings-i18n';
import {cancelAiSuggestion,createAiChat,deleteAiChat,errorCode,getAiChat,listAiEntries,listAiWorkspace,openExternal,readAiDroppedImages,renameAiChat,sendAiSuggestion,type AiChatSummary,type AiCustomPreset,type AiEntry,type AiImage,type AiPersonalization,type AiStoredTurn,type AiSuggestionResponse} from './backend';
import {getBuiltinAiPresets} from './ai-presets';
import {aiSuggestionsText} from './ai-suggestions-i18n';
import AiPresetEditor from './AiPresetEditor';
import Preview from './Preview';
import {isTauri} from '@tauri-apps/api/core';
import {getCurrentWebview} from '@tauri-apps/api/webview';
import './ai-suggestions.css';

const MAX_IMAGES=4;
const MAX_IMAGE_BYTES=2*1024*1024;
const encoder=new TextEncoder();

/** Re-encoding strips original image metadata before upload or local chat storage. */
async function normalizeImage(file:File):Promise<AiImage> {
 if(!['image/jpeg','image/png','image/webp'].includes(file.type))throw new Error('ai_image_invalid');
 if(file.size>20*1024*1024)throw new Error('ai_image_too_large');
 let image:ImageBitmap;
 try{image=await createImageBitmap(file,{imageOrientation:'from-image'});}catch{throw new Error('ai_image_invalid');}
 try{
  if(!image.width||!image.height||image.width*image.height>64_000_000)throw new Error('ai_image_too_large');
  const scale=Math.min(1,1536/Math.max(image.width,image.height));
  const canvas=document.createElement('canvas');canvas.width=Math.max(1,Math.round(image.width*scale));canvas.height=Math.max(1,Math.round(image.height*scale));
  const context=canvas.getContext('2d');if(!context)throw new Error('ai_image_invalid');
  const encode=(mime:'image/jpeg'|'image/png',quality?:number)=>new Promise<Blob>((resolve,reject)=>canvas.toBlob(blob=>blob?resolve(blob):reject(new Error('ai_image_invalid')),mime,quality));
  let mime:AiImage['mimeType']=file.type==='image/png'?'image/png':'image/jpeg';
  if(mime==='image/jpeg'){context.fillStyle='#fff';context.fillRect(0,0,canvas.width,canvas.height);}
  context.drawImage(image,0,0,canvas.width,canvas.height);let blob=await encode(mime,.86);
  if(blob.size>MAX_IMAGE_BYTES){mime='image/jpeg';context.fillStyle='#fff';context.fillRect(0,0,canvas.width,canvas.height);context.drawImage(image,0,0,canvas.width,canvas.height);blob=await encode(mime,.75);}
  if(blob.size>MAX_IMAGE_BYTES)throw new Error('ai_image_too_large');
  const data=await new Promise<string>((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>typeof reader.result==='string'?resolve(reader.result.split(',')[1]??''):reject(new Error('ai_image_invalid'));reader.onerror=()=>reject(new Error('ai_image_invalid'));reader.readAsDataURL(blob);});
  if(!data)throw new Error('ai_image_invalid');return {mimeType:mime,dataBase64:data};
 }finally{image.close();}
}

type SearchScope='library'|'online'|'both';
type Store='booth'|'gumroad'|'jinxxy';
type DeleteState={id:string;ready:boolean};
const imageSource=(image:AiImage)=>`data:${image.mimeType};base64,${image.dataBase64}`;
const sortChats=(chats:AiChatSummary[])=>[...chats].sort((a,b)=>b.updatedAt.localeCompare(a.updatedAt));
function AiCatAvatar(){return <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.65" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M5 9 4.5 4.8Q4.4 3.8 5.4 4.2L9 6.1Q12 5.2 15 6.1L18.6 4.2Q19.6 3.8 19.5 4.8L19 9Q21 11 21 14.5Q21 21 12 21Q3 21 3 14.5Q3 11 5 9Z"/></svg>;}

export interface AiSuggestionsProps {locale:Locale;active?:boolean;onOpenProduct:(productId:string)=>void;avatarOptions?:{id:string;label:string}[];initialAvatarId?:string;onUpdateBlockedChange?:(blocked:boolean)=>void}

export default function AiSuggestions({locale,active=true,onOpenProduct,avatarOptions=[],initialAvatarId='',onUpdateBlockedChange}:AiSuggestionsProps) {
 const L=(key:string)=>aiSuggestionsText(locale,key);
 const [presetId,setPresetId]=useState('chat');
 const [historyMode,setHistoryMode]=useState(false);
 const [chats,setChats]=useState<AiChatSummary[]>([]);
 const [chatId,setChatId]=useState<string|null>(null);
 const [customPresets,setCustomPresets]=useState<AiCustomPreset[]>([]);
 const [personalization,setPersonalization]=useState<AiPersonalization>({avatarDataUrl:null,userName:'',replyLanguage:'app',personality:'basic'});
 const [editor,setEditor]=useState<{preset:AiCustomPreset|null}|null>(null);
 const [scope,setScope]=useState<SearchScope>('library');
 const [stores,setStores]=useState<Store[]>(['booth','gumroad','jinxxy']);
 const [category,setCategory]=useState('');
 const [avatarId,setAvatarId]=useState(initialAvatarId);
 const [prompt,setPrompt]=useState('');
 const [images,setImages]=useState<AiImage[]>([]);
 const [turns,setTurns]=useState<AiStoredTurn[]>([]);
 const [entries,setEntries]=useState<AiEntry[]>([]);
 const [entryId,setEntryId]=useState('');
 const [loading,setLoading]=useState(true);
 const [chatLoading,setChatLoading]=useState(false);
 const [busy,setBusy]=useState(false);
 const [imageBusy,setImageBusy]=useState(false);
 const [dropActive,setDropActive]=useState(false);
 const [imageTrayOpen,setImageTrayOpen]=useState(false);
 const composing=useRef(false);
 const [error,setError]=useState('');
 const [status,setStatus]=useState('');
 const [renaming,setRenaming]=useState<string|null>(null);
 const [renameDraft,setRenameDraft]=useState('');
 const [deleting,setDeleting]=useState<DeleteState|null>(null);
 const [historyBusy,setHistoryBusy]=useState(false);
 useEffect(()=>{onUpdateBlockedChange?.(!!prompt||!!images.length||!!editor||!!renaming||!!deleting||busy||imageBusy||historyBusy);},[prompt,images.length,editor,renaming,deleting,busy,imageBusy,historyBusy,onUpdateBlockedChange]);
 useEffect(()=>()=>{onUpdateBlockedChange?.(false);},[onUpdateBlockedChange]);
 const currentRequest=useRef<string|null>(null);
 const cancelRequested=useRef(false);
 const requestSubmitted=useRef(false);
 const conversion=useRef(0);
 const imageLock=useRef(false);
 const mounted=useRef(true);
 const navigation=useRef(0);
 const deleteTimer=useRef<ReturnType<typeof setTimeout>|null>(null);
 const deleteButton=useRef<HTMLButtonElement>(null);
 const imageInput=useRef<HTMLInputElement>(null);
 const main=useRef<HTMLDivElement>(null);
 const messages=useRef<HTMLDivElement>(null);
 const latest=useRef({active,busy,loading,images,entries,entryId});latest.current={active,busy,loading,images,entries,entryId};
 const presets=[...getBuiltinAiPresets(locale),...customPresets.map(preset=>({...preset,builtin:false}))];
 const selectedPreset=presets.find(preset=>preset.id===presetId)??{title:L('customPreset'),description:L('customPresetDescription')};
 const provider=entries.find(entry=>entry.id===entryId);
 const vision=provider?.visionSupported===true;
 const keyMissing=!!provider&&provider.keyRequired&&!provider.keyPresent;
 const hasOnline=scope!=='library';
 const promptTooLong=encoder.encode(prompt.trim()).length>4000;
 const locked=loading||busy||chatLoading||historyBusy;
 const canSend=active&&!loading&&!!provider&&!locked&&!imageBusy&&!keyMissing&&!promptTooLong&&(!images.length||vision)&&(!!prompt.trim()||images.length>0)&&(!hasOnline||stores.length>0);

 useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;conversion.current++;navigation.current++;if(deleteTimer.current)clearTimeout(deleteTimer.current);const id=currentRequest.current;currentRequest.current=null;if(id)void cancelAiSuggestion(id).catch(()=>{});};},[]);
 useEffect(()=>{
  if(!active)return;let current=true;setLoading(true);
  void Promise.all([listAiWorkspace(),listAiEntries()]).then(([workspace,providers])=>{
   if(!current)return;setChats(sortChats(workspace.chats));setCustomPresets(workspace.presets);setPersonalization(workspace.personalization);setEntries(providers.entries);
   setEntryId(previous=>providers.entries.some(entry=>entry.id===previous)?previous:providers.activeEntryId??providers.entries[0]?.id??'');
  }).catch(value=>{if(current)setError(errorCode(value));}).finally(()=>{if(current)setLoading(false);});return()=>{current=false;};
 },[active]);
 useEffect(()=>{
  const chatMessages=messages.current;chatMessages?.scrollTo({top:turns.length||busy?chatMessages.scrollHeight:0});
  if(!turns.length&&!busy&&!chatLoading&&window.matchMedia('(max-height:350px)').matches&&main.current&&chatMessages)main.current.scrollTo({top:chatMessages.offsetTop-main.current.offsetTop});
 },[turns,busy,chatLoading]);
 useEffect(()=>{if(!images.length)setImageTrayOpen(false);},[images.length]);
 useEffect(()=>{if(!active)setDropActive(false);},[active]);
 useEffect(()=>{if(deleting?.ready)deleteButton.current?.focus();},[deleting?.id,deleting?.ready]);
 useEffect(()=>{
  let disposed=false;let unlisten:(()=>void)|undefined;
  if(active&&isTauri())void getCurrentWebview().onDragDropEvent(event=>{
   if(event.payload.type==='enter'||event.payload.type==='over'){setDropActive(true);return;}
   setDropActive(false);if(event.payload.type!=='drop')return;
   const paths=event.payload.paths;
   const state=latest.current;const selected=state.entries.find(entry=>entry.id===state.entryId);
   if(!state.active||state.busy||state.loading||imageLock.current)return;
   if(!selected?.visionSupported){setError('ai_images_unavailable');return;}
   if(paths.length+state.images.length>MAX_IMAGES){setError('ai_image_limit');return;}
   imageLock.current=true;setImageBusy(true);setError('');const epoch=conversion.current;
   void readAiDroppedImages(paths).then(value=>{if(mounted.current&&epoch===conversion.current)setImages(previous=>[...previous,...value]);}).catch(value=>{if(mounted.current&&epoch===conversion.current)setError(errorCode(value));}).finally(()=>{imageLock.current=false;if(mounted.current)setImageBusy(false);});
  }).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{});
  return()=>{disposed=true;unlisten?.();};
 },[active]);

 function updateSummary(chat:AiChatSummary){setChats(previous=>sortChats([...previous.filter(value=>value.id!==chat.id),chat]));}
 function resetConfirmation(){if(deleteTimer.current)clearTimeout(deleteTimer.current);deleteTimer.current=null;setDeleting(null);setRenaming(null);}
 function newChat(nextPreset=presetId){if(locked)return;main.current?.scrollTo({top:0});navigation.current++;resetConfirmation();setPresetId(presets.some(preset=>preset.id===nextPreset)?nextPreset:'chat');setChatId(null);setTurns([]);setError('');setStatus('');setHistoryMode(true);}
 async function openChat(id:string){
  if(locked)return;main.current?.scrollTo({top:0});resetConfirmation();setChatLoading(true);setError('');setStatus('');const epoch=++navigation.current;
  try{const value=await getAiChat(id);if(mounted.current&&epoch===navigation.current){setChatId(value.chat.id);setPresetId(value.chat.presetId);setTurns(value.messages);setHistoryMode(true);}}
  catch(value){if(mounted.current&&epoch===navigation.current)setError(errorCode(value));}
  finally{if(mounted.current&&epoch===navigation.current)setChatLoading(false);}
 }
 async function renameChat(chat:AiChatSummary){
  const title=renameDraft.trim();if(historyBusy||!title)return;setHistoryBusy(true);setError('');
  try{const value=await renameAiChat(chat.id,title);if(mounted.current){updateSummary(value);setRenaming(null);}}
  catch(value){if(mounted.current)setError(errorCode(value));}
  finally{if(mounted.current)setHistoryBusy(false);}
 }
 async function deleteChat(chat:AiChatSummary){
  if(locked)return;
  if(deleting?.id!==chat.id){resetConfirmation();setDeleting({id:chat.id,ready:false});deleteTimer.current=setTimeout(()=>{if(mounted.current)setDeleting(value=>value?.id===chat.id?{id:chat.id,ready:true}:value);deleteTimer.current=null;},600);return;}
  if(!deleting.ready)return;setHistoryBusy(true);setError('');
  try{await deleteAiChat(chat.id);if(mounted.current){setChats(previous=>previous.filter(value=>value.id!==chat.id));if(chatId===chat.id){setChatId(null);setTurns([]);}resetConfirmation();}}
  catch(value){if(mounted.current)setError(errorCode(value));}
  finally{if(mounted.current)setHistoryBusy(false);}
 }
 async function addImages(files:File[]) {
  if(!active||loading||busy||imageLock.current)return;
  if(!vision){setError('ai_images_unavailable');return;}
  const accepted=files.filter(file=>file.type.startsWith('image/'));if(!accepted.length){setError('ai_image_invalid');return;}
  if(accepted.length+images.length>MAX_IMAGES){setError('ai_image_limit');return;}
  imageLock.current=true;setImageBusy(true);setError('');const epoch=conversion.current;
  try{const normalized:AiImage[]=[];for(const file of accepted)normalized.push(await normalizeImage(file));if(mounted.current&&epoch===conversion.current)setImages(previous=>[...previous,...normalized]);}
  catch(value){if(mounted.current&&epoch===conversion.current)setError(errorCode(value));}
  finally{imageLock.current=false;if(mounted.current)setImageBusy(false);}
 }
 const drop=(event:DragEvent<HTMLElement>)=>{if(event.dataTransfer.types.includes('Files')){event.preventDefault();setDropActive(false);void addImages(Array.from(event.dataTransfer.files));}};
 const paste=(event:ClipboardEvent<HTMLElement>)=>{const files=Array.from(event.clipboardData.items).filter(item=>item.kind==='file'&&item.type.startsWith('image/')).map(item=>item.getAsFile()).filter((file):file is File=>!!file);if(files.length){event.preventDefault();void addImages(files);}};
 async function cancel(){const id=currentRequest.current;if(!id)return;cancelRequested.current=true;setStatus('ai_chat_cancelled');if(!requestSubmitted.current)return;try{await cancelAiSuggestion(id);}catch(value){if(mounted.current)setError(errorCode(value));}}
 async function send(){
  if(!canSend||currentRequest.current)return;
  const requestId=crypto.randomUUID();currentRequest.current=requestId;cancelRequested.current=false;requestSubmitted.current=false;setBusy(true);setError('');setStatus('');resetConfirmation();setHistoryMode(true);
  const content=prompt.trim();const attached=[...images];const sentAt=new Date().toISOString();let sessionId=chatId;let accepted=false;
  try{
   if(!sessionId){const detail=await createAiChat(presetId);sessionId=detail.chat.id;if(!mounted.current||currentRequest.current!==requestId)return;setChatId(sessionId);updateSummary(detail.chat);}
   if(cancelRequested.current)return;
   setTurns(previous=>[...previous,{id:requestId,role:'user',content,images:attached,createdAt:sentAt,status:'pending'}]);setPrompt('');setImages([]);setImageTrayOpen(false);if(imageInput.current)imageInput.current.value='';accepted=true;requestSubmitted.current=true;
   await sendAiSuggestion({requestId,chatId:sessionId,entryId,appLanguage:locale,prompt:content,preset:'chat',scope,stores:hasOnline?[...stores]:[],...(category?{category}:{}),...(avatarId?{avatarId}:{}),images:attached});
  }catch(value){if(mounted.current&&currentRequest.current===requestId)setError(errorCode(value));}
  finally{
   if(mounted.current&&currentRequest.current===requestId){
    if(sessionId){
     try{const detail=await getAiChat(sessionId);if(mounted.current&&currentRequest.current===requestId){setTurns(detail.messages);updateSummary(detail.chat);if(accepted&&!detail.messages.some(turn=>turn.role==='user'&&Date.parse(turn.createdAt)>=Date.parse(sentAt))){setPrompt(content);setImages(attached);}}}
     catch(value){if(mounted.current){setError(errorCode(value));setTurns(previous=>previous.map(turn=>turn.id===requestId?{...turn,status:'failed'}:turn));}}
    }
    currentRequest.current=null;requestSubmitted.current=false;setBusy(false);
   }
  }
 }
 async function openPage(url:string){try{await openExternal(url);}catch(value){if(mounted.current)setError(errorCode(value));}}
 const storeLabel=(store:string)=>store==='booth'?'BOOTH':store==='gumroad'?'Gumroad':store==='jinxxy'?'Jinxxy':store==='local'?L('library'):store;
 const apiLabel=(entry:AiEntry)=>{const label=`${entry.provider} · ${entry.model}`;if(entries.filter(value=>value.provider===entry.provider&&value.model===entry.model).length<2)return label;let host='';try{host=new URL(entry.endpoint).host;}catch{}return `${label} · ${host||entry.id.slice(0,6)}${entries.filter(value=>value.provider===entry.provider&&value.model===entry.model&&value.endpoint===entry.endpoint).length>1?' · '+entry.id.slice(0,6):''}`;};
 const resultCard=(result:AiSuggestionResponse['recommendations'][number])=>{
  const source=result.thumbnailPath||result.thumbnailUrl;
  const body=<><div className="ai-chat-result-image">{source?<Preview src={source} title={result.title} lazy/>:<ImagePlus size={25}/>}<span>{storeLabel(result.provider)}</span></div><div className="ai-chat-result-body"><strong>{result.title}</strong>{result.reason&&<small>{result.reason}</small>}</div></>;
  return result.origin==='library'&&result.productId?<button type="button" className="ai-chat-result" key={result.candidateId} title={L('openProduct')} onClick={()=>onOpenProduct(result.productId!)}>{body}</button>:<div className="ai-chat-result" key={result.candidateId}>{body}{result.url&&<button type="button" className="ai-chat-result-action" onClick={()=>void openPage(result.url!)}><ExternalLink size={13}/>{L('openPage')}</button>}</div>;
 };
 const date=(value:string)=>{const parsed=new Date(value);return Number.isNaN(parsed.getTime())?'':new Intl.DateTimeFormat(locale,{month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'}).format(parsed);};
 const chatTitle=(chat:AiChatSummary)=>chat.title===chat.presetId?presets.find(preset=>preset.id===chat.presetId&&preset.builtin)?.title??chat.title:chat.title||L('newChat');
 const presetButton=(preset:typeof presets[number])=><div className="ai-preset-row" key={preset.id}><button type="button" className={'ai-preset-choice '+(presetId===preset.id?'selected':'')} disabled={locked} onClick={()=>newChat(preset.id)}><span><strong>{preset.title}</strong><small>{preset.description}</small></span><ChevronRight size={15}/></button>{!preset.builtin&&<button type="button" className="ai-preset-edit icon-button" disabled={locked} aria-label={`${L('editPreset')} ${preset.title}`} onClick={()=>setEditor({preset})}><Pencil size={14}/></button>}</div>;

 return <section className={'ai-chat '+(dropActive?'ai-drop-active':'')} onDragOver={event=>{if(event.dataTransfer.types.includes('Files')){event.preventDefault();setDropActive(true);}}} onDragLeave={event=>{if(!(event.relatedTarget instanceof Node)||!event.currentTarget.contains(event.relatedTarget))setDropActive(false);}} onDrop={drop} onPaste={paste}>
  {dropActive&&<div className="ai-drop-indicator" role="status"><ImagePlus size={24}/>{L('dropImages')}</div>}
  <aside className="ai-chat-sidebar" aria-label={L(historyMode?'chatHistory':'presets')}>
   {historyMode?<>
    <button type="button" className="ai-chat-back subtle" disabled={locked} onClick={()=>{resetConfirmation();setHistoryMode(false);}}><ArrowLeft size={15}/>{L('backPresets')}</button>
    <button type="button" className="primary ai-new-chat" disabled={locked} onClick={()=>newChat()}><Plus size={16}/>{L('newChat')}</button>
    <div className="ai-chat-history">
     {chats.map(chat=><div className={'ai-history-row '+(chatId===chat.id?'selected ':'')+(deleting?.id===chat.id?'confirming':'')} key={chat.id} onKeyDown={event=>{if(event.key==='Escape'){event.stopPropagation();resetConfirmation();}}}>
      {renaming===chat.id?<form className="ai-history-rename" onSubmit={event=>{event.preventDefault();void renameChat(chat);}}><input autoFocus aria-label={L('chatTitle')} value={renameDraft} disabled={historyBusy} maxLength={160} onChange={event=>setRenameDraft(event.target.value)} onKeyDown={event=>{if(event.key==='Escape'){event.stopPropagation();setRenaming(null);}}}/><button type="submit" disabled={historyBusy||!renameDraft.trim()}>{L('save')}</button><button type="button" className="icon-button" aria-label={L('cancel')} disabled={historyBusy} onClick={()=>setRenaming(null)}><X size={13}/></button></form>:<>
       <button type="button" className="ai-history-open" title={chatTitle(chat)} disabled={locked||deleting?.id===chat.id} onClick={()=>void openChat(chat.id)}><MessageCircle size={16}/><span><strong>{chatTitle(chat)}</strong><small>{date(chat.updatedAt)}</small></span></button>
       <div className="ai-history-actions"><button type="button" className="icon-button" disabled={locked||deleting?.id===chat.id} aria-label={`${L('renameChat')} ${chatTitle(chat)}`} onClick={()=>{resetConfirmation();setRenaming(chat.id);setRenameDraft(chatTitle(chat));}}><Pencil size={14}/></button><button ref={deleting?.id===chat.id?deleteButton:undefined} type="button" className={'ai-history-delete '+(deleting?.id===chat.id?'expanded':'')} disabled={locked||(deleting?.id===chat.id&&!deleting.ready)} aria-label={deleting?.id===chat.id?L('confirmDeleteChat'):`${L('deleteChat')} ${chatTitle(chat)}`} onKeyDown={event=>{if(event.repeat&&(event.key==='Enter'||event.key===' '))event.preventDefault();}} onClick={()=>void deleteChat(chat)}><span className="ai-delete-text">{L('confirmDeleteChat')}</span><Trash2 size={14}/></button></div>
      </>}
     </div>)}
     {!chats.length&&!loading&&<div className="ai-chat-note ai-history-empty">{L('noChats')}</div>}
    </div>
   </>:<>
    <div className="ai-chat-free">{presets[0]&&presetButton(presets.find(preset=>preset.id==='chat')??presets[0])}</div>
    <div className="ai-chat-presets">{presets.filter(preset=>preset.id!=='chat').map(presetButton)}</div>
    <button type="button" className="ai-add-preset" disabled={locked} onClick={()=>setEditor({preset:null})}><Plus size={16}/>{L('addPreset')}</button>
   </>}
  </aside>
  <div className="ai-chat-main" ref={main}>
   <div className="ai-chat-options">
    <label>{L('scope')}<select value={scope} title={L(scope)} disabled={locked} onChange={event=>setScope(event.target.value as SearchScope)}>{(['library','online','both'] as const).map(value=><option value={value} key={value}>{L(value)}</option>)}</select></label>
    {hasOnline&&<div className="ai-chat-stores" role="group" aria-label={L('stores')}>{(['booth','gumroad','jinxxy'] as const).map(store=><label key={store}><input type="checkbox" checked={stores.includes(store)} disabled={locked} onChange={event=>setStores(previous=>event.target.checked?[...previous,store]:previous.filter(value=>value!==store))}/>{storeLabel(store)}</label>)}</div>}
    <label>{L('category')}<select value={category} disabled={locked} onChange={event=>setCategory(event.target.value)}><option value="">{L('anyCategory')}</option>{categoryKeys.map(value=><option value={value} key={value}>{t(locale,value)}</option>)}</select></label>
    {avatarOptions.length>0&&<label>{L('avatar')}<select value={avatarId} disabled={locked} onChange={event=>setAvatarId(event.target.value)}><option value="">{L('anyAvatar')}</option>{avatarOptions.map(avatar=><option value={avatar.id} key={avatar.id}>{avatar.label}</option>)}</select></label>}
   </div>
   <div className="ai-chat-messages" ref={messages} role="log" aria-label={L('chat')}>
    {!turns.length&&!busy&&!chatLoading&&<div className="ai-preset-intro"><MessageCircle size={28}/><h2>{selectedPreset?.title}</h2><p>{selectedPreset?.description}</p></div>}
    {turns.map(turn=><article className={'ai-chat-turn '+(turn.role==='user'?'ai-chat-user':'ai-chat-assistant')} key={turn.id}>
     <div className="ai-chat-avatar" aria-hidden="true">{turn.role==='user'?(personalization.avatarDataUrl?<img src={personalization.avatarDataUrl} alt=""/>:<User size={20}/>):<AiCatAvatar/>}</div>
     <div className="ai-chat-bubble"><header><strong>{turn.role==='user'?personalization.userName||L('user'):L('assistant')}</strong><time dateTime={turn.createdAt}>{date(turn.createdAt)}</time></header>
      {turn.images?.length>0&&<div className="ai-sent-images">{turn.images.map((image,index)=><img src={imageSource(image)} key={index} alt={`${L('image')} ${index+1}`}/>)}</div>}
      {turn.content&&<div className="ai-chat-message">{turn.content}</div>}
      {turn.status==='pending'&&<div className="ai-chat-note" role="status">{L('sending')}</div>}
      {turn.status&&turn.status!=='sent'&&turn.status!=='pending'&&<div className="ai-chat-error">{L(turn.status==='cancelled'?'ai_chat_cancelled':'ai_chat_failed')}</div>}
      {(['library','store'] as const).map(origin=>{const recommendations=turn.response?.recommendations.filter(result=>result.origin===origin)??[];return recommendations.length>0&&<div key={origin}><div className="ai-chat-result-heading">{L(origin==='library'?'library':'online')}</div><div className="ai-chat-results">{recommendations.map(resultCard)}</div></div>;})}
      {turn.response?.searchStatus.filter(item=>item.status!=='ok').map(item=><div className="ai-chat-note" key={item.provider}>{storeLabel(item.provider)} · {L(item.status==='unavailable'?'storeUnavailable':'noResults')}</div>)}
     </div>
    </article>)}
    {(busy||chatLoading)&&<div className="ai-chat-note" role="status">{L('thinking')}</div>}
   </div>
   <form className="ai-chat-composer" onSubmit={event=>{event.preventDefault();void send();}}>
    <div className="ai-chat-composer-row">
     <div className="ai-chat-image-zone"><input ref={imageInput} type="file" accept="image/jpeg,image/png,image/webp" multiple hidden onChange={event=>{const files=Array.from(event.target.files??[]);event.target.value='';void addImages(files);}}/>
      <button type="button" className="ai-add-images" disabled={loading||busy||imageBusy||!vision||images.length>=MAX_IMAGES} title={!vision?L('ai_images_unavailable'):L('addImages')} onClick={()=>imageInput.current?.click()}>{images.length?<img src={imageSource(images[0])} alt={L('image')}/>:<ImagePlus size={24}/>}<span>{L('addImages')}</span></button>
      {images.length>0&&<button type="button" className="ai-image-count" aria-label={L('attachments')} aria-expanded={imageTrayOpen} onClick={()=>setImageTrayOpen(value=>!value)}>{images.length}</button>}
      {images.length>0&&imageTrayOpen&&<div className="ai-chat-images">{images.map((image,index)=><div className="ai-chat-image" key={index}><img src={imageSource(image)} alt={`${L('image')} ${index+1}`}/><button type="button" disabled={busy||imageBusy} aria-label={`${L('removeImage')} ${index+1}`} onClick={()=>setImages(previous=>previous.filter((_,position)=>position!==index))}><X size={12}/></button></div>)}</div>}
     </div>
     <textarea aria-label={L('prompt')} aria-invalid={promptTooLong} placeholder={L('promptPlaceholder')} value={prompt} disabled={busy||chatLoading} maxLength={4000} onChange={event=>setPrompt(event.target.value)} onCompositionStart={()=>{composing.current=true;}} onCompositionEnd={()=>{composing.current=false;}} onKeyDown={event=>{if(event.key==='Enter'&&!event.shiftKey&&!composing.current&&!event.nativeEvent.isComposing&&event.nativeEvent.keyCode!==229){event.preventDefault();void send();}}}/>
     <div className="ai-chat-composer-actions">{busy?<button type="button" className="ai-chat-send" onClick={()=>void cancel()}><Square size={13}/>{L('cancel')}</button>:<button type="submit" className="primary ai-chat-send" disabled={!canSend}><Send size={15}/>{L('send')}</button>}<select aria-label={L('api')} title={provider?apiLabel(provider):L('selectApi')} value={entryId} disabled={loading||busy} onChange={event=>setEntryId(event.target.value)}>{!entries.length&&<option value="">{L('selectApi')}</option>}{entries.map(entry=><option key={entry.id} value={entry.id}>{apiLabel(entry)}</option>)}</select></div>
    </div>
    {!loading&&!entries.length&&<div className="ai-chat-error" role="alert">{aiSettingsText(locale,'ai_not_configured')}</div>}{keyMissing&&<div className="ai-chat-error" role="alert">{L('ai_key_missing')}</div>}
    {!loading&&!vision&&images.length>0&&<div className="ai-chat-note" role="status">{L('ai_images_unavailable')}</div>}{hasOnline&&!stores.length&&<div className="ai-chat-note">{L('ai_online_scope_empty')}</div>}
    {promptTooLong&&<div className="ai-chat-error" role="alert">{L('ai_prompt_too_long')}</div>}{error&&<div className="ai-chat-error" role="alert">{L(error)}</div>}{status&&<div className="ai-chat-note" role="status">{L(status)}</div>}
   </form>
  </div>
  {editor&&<AiPresetEditor key={editor.preset?.id??'new'} locale={locale} preset={editor.preset} onSaved={value=>{setCustomPresets(previous=>[...previous.filter(preset=>preset.id!==value.id),value]);}} onClose={()=>setEditor(null)}/>}
 </section>;
}
