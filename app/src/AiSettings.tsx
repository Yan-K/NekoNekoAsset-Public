import {useEffect,useRef,useState} from 'react';
import {Plus,Trash2,UserRound} from 'lucide-react';
import {errorCode,getAiProviders,listAiEntries,listAiWorkspace,saveAiPersonalization,testAiDraft,saveAiEntry,selectAiEntry,removeAiEntry,testAiEntry,startAi,cancelAi,retryAi,saveAiPreferences,type AiEntryList,type AiProviderPreset,type AiScopes,type NativeState} from './backend';
import type {Locale} from './model';
import Toggle from './Toggle';
import {aiSettingsText} from './ai-settings-i18n';
import {DEFAULT_AI_PERSONALIZATION,getAiPersonalities,normalizeAiAvatar,type AiPersonalization} from './ai-personalization';
import './AiSettings.css';

export interface AiSettingsProps {locale:Locale;nativeState?:NativeState;onUpdateBlockedChange?:(blocked:boolean)=>void}
function savedScopes(value:unknown):AiScopes {
 const saved=value&&typeof value==='object'?value as Partial<AiScopes>:{};
 return {title:saved.title!==false,tags:saved.tags!==false,description:saved.description!==false,files:saved.files!==false,images:saved.images===true};
}
export default function AiSettings({locale,nativeState,onUpdateBlockedChange}:AiSettingsProps) {
 const L=(key:string)=>aiSettingsText(locale,key);
 const [list,setList]=useState<AiEntryList>({entries:[],activeEntryId:null}),[presets,setPresets]=useState<AiProviderPreset[]>([]);
 const [loading,setLoading]=useState(true),[busy,setBusy]=useState(false),[error,setError]=useState(''),[result,setResult]=useState(''),[testing,setTesting]=useState(false);
 const [editing,setEditing]=useState(false),[provider,setProvider]=useState('openai'),[endpoint,setEndpoint]=useState('https://api.openai.com/v1'),[model,setModel]=useState('gpt-4o-mini'),[hasKey,setHasKey]=useState(false),[proof,setProof]=useState(''),[jobId,setJobId]=useState('');
 const [personalization,setPersonalization]=useState<AiPersonalization>({...DEFAULT_AI_PERSONALIZATION}),[savedPersonalization,setSavedPersonalization]=useState<AiPersonalization>({...DEFAULT_AI_PERSONALIZATION});
 const [personalizationLoaded,setPersonalizationLoaded]=useState(false);
 const avatarInput=useRef<HTMLInputElement>(null);
 const keyInput=useRef<HTMLInputElement>(null),lock=useRef(false),mounted=useRef(true),draftRevision=useRef(0);
 const prefs=nativeState?.preferences,scopes=savedScopes(prefs?.aiScopes),auto=prefs?.aiEnabled===true,featuresEnabled=prefs?.aiFeaturesEnabled===true;
 const selected=list.entries.find(entry=>entry.id===list.activeEntryId),ready=!!selected&&(!selected.keyRequired||selected.keyPresent);
 const jobsActive=(nativeState?.jobs??[]).some(job=>job.type.startsWith('ai:')&&['queued','running'].includes(job.state));
 const job=nativeState?.jobs.find(value=>value.id===jobId),jobActive=!!jobId&&(!job||['queued','running'].includes(job.state));
 const disabled=loading||busy;
 const personalizationDirty=JSON.stringify(personalization)!==JSON.stringify(savedPersonalization);
 useEffect(()=>{onUpdateBlockedChange?.(editing||personalizationDirty||busy||testing);},[editing,personalizationDirty,busy,testing,onUpdateBlockedChange]);
 useEffect(()=>()=>{onUpdateBlockedChange?.(false);},[onUpdateBlockedChange]);
 const personalizationDisabled=disabled||editing||!personalizationLoaded;
 const invalidate=()=>{draftRevision.current++;setProof('');setResult('');};
 const clearKey=()=>{if(keyInput.current)keyInput.current.value='';setHasKey(false);invalidate();};
 useEffect(()=>{
  mounted.current=true;
  void Promise.all([listAiEntries(),getAiProviders(),listAiWorkspace()]).then(([entries,providers,workspace])=>{if(mounted.current){setList(entries);setPresets(providers.presets);setPersonalization(workspace.personalization);setSavedPersonalization(workspace.personalization);setPersonalizationLoaded(true);}}).catch(value=>{if(mounted.current)setError(errorCode(value));}).finally(()=>{if(mounted.current)setLoading(false);});
  return()=>{mounted.current=false;draftRevision.current++;if(keyInput.current)keyInput.current.value='';};
 },[]);
 useEffect(()=>{const input=keyInput.current;return()=>{if(input)input.value='';};},[editing]);
 const run=async(action:()=>Promise<void>)=>{
  if(lock.current)return;lock.current=true;setBusy(true);setError('');setResult('');
  try{await action();}catch(value){if(mounted.current)setError(errorCode(value));}finally{lock.current=false;if(mounted.current)setBusy(false);}
 };
 const accept=(entries:AiEntryList)=>{if(mounted.current){setList(entries);invalidate();}};
 const openEditor=()=>{
  const preset=presets.find(value=>value.id==='openai')??presets[0];
  setProvider(preset?.id??'openai');setEndpoint(preset?.endpoint??'https://api.openai.com/v1');setModel(preset?.model??'gpt-4o-mini');clearKey();setError('');setEditing(true);
 };
 const cancelEditor=()=>{clearKey();setEditing(false);setError('');};
 const chooseProvider=(id:string)=>{const preset=presets.find(value=>value.id===id);if(!preset)return;setProvider(id);setEndpoint(preset.endpoint);setModel(preset.model);clearKey();};
 const draft=()=>({provider,endpoint,model,key:keyInput.current?.value??''});
 const keyRequired=presets.find(value=>value.id===provider)?.keyRequired!==false;
 const canTest=!!endpoint.trim()&&!!model.trim()&&(!keyRequired||hasKey);
 const connectionStatus=(error||result==='draftTested'||result==='aiConnected'||testing)?<div className={'ai-status'+(error?' error':testing?'':' success')} role={error?'alert':'status'}>{L(error||(testing?'aiTesting':result))}</div>:null;
 const testDraft=()=>void run(async()=>{
  setTesting(true);const revision=draftRevision.current;
  try{const value=await testAiDraft(draft());if(mounted.current&&revision===draftRevision.current){setProof(value.testToken);setResult('draftTested');}}finally{if(mounted.current)setTesting(false);}
 });
 const save=()=>{if(!proof||!canTest)return;void run(async()=>{
  try{const value=await saveAiEntry({...draft(),testToken:proof});accept(value);if(mounted.current)setEditing(false);}finally{if(mounted.current)clearKey();else if(keyInput.current)keyInput.current.value='';}
 });};
 const select=(id:string)=>{if(disabled||editing||jobsActive||id===list.activeEntryId)return;void run(async()=>accept(await selectAiEntry(id)));};
 const remove=()=>{if(!selected||disabled||editing||jobsActive)return;void run(async()=>{
  try{accept(await removeAiEntry(selected.id));}catch(value){try{accept(await listAiEntries());}catch{}throw value;}
 });};
 const testSaved=()=>{if(!selected||!ready||disabled||editing)return;void run(async()=>{setTesting(true);try{await testAiEntry(selected.id);if(mounted.current)setResult('aiConnected');}finally{if(mounted.current)setTesting(false);}});};
 const savePrefs=(value:AiScopes,enabled=auto)=>void run(async()=>{await saveAiPreferences({aiEnabled:enabled,aiScopes:value});});
 const updatePersonalization=(patch:Partial<AiPersonalization>)=>{setPersonalization(value=>({...value,...patch}));setResult('');};
 const chooseAvatar=(file:File)=>void run(async()=>{const dataUrl=await normalizeAiAvatar(file);if(mounted.current)setPersonalization(value=>({...value,avatarDataUrl:dataUrl}));});
 const savePersonalization=()=>void run(async()=>{
  const value=await saveAiPersonalization({...personalization,userName:personalization.userName.trim()});
  if(mounted.current){setPersonalization(value);setSavedPersonalization(value);setResult('personalizationSaved');}
 });
 return <div className="wf-scroll ai-settings">
  <div className="ai-entry-list" role="listbox" aria-label={L('entries')} aria-busy={loading}>
   {!loading&&!list.entries.length&&<span className="ai-entry-empty">{L('emptyEntries')}</span>}
   {list.entries.map(entry=><button type="button" className="ai-entry" role="option" aria-selected={entry.id===list.activeEntryId} disabled={disabled||editing||jobsActive} key={entry.id} title={[presets.find(value=>value.id===entry.provider)?.label??entry.provider,entry.model,entry.endpoint].join('\n')} onClick={()=>select(entry.id)}>
    <span className="ai-entry-provider">{presets.find(value=>value.id===entry.provider)?.label??entry.provider}</span><span className="ai-entry-model">{entry.model}</span>{entry.keyPresent&&<span className="ai-entry-saved" aria-label={L('aiSaved')} title={L('aiSaved')}>✓</span>}
   </button>)}
  </div>
  <div className="form-row"><button type="button" className="ai-add" disabled={disabled||editing||jobsActive} onClick={openEditor}><Plus/>{L('addEntry')}</button><button type="button" className="danger" disabled={disabled||editing||jobsActive||!selected} onClick={remove}><Trash2/>{L('remove')}</button><button type="button" disabled={disabled||editing||!ready} onClick={testSaved}>{L('test')}</button></div>
  {!editing&&connectionStatus}
  {editing&&<form className="ai-editor" onSubmit={event=>{event.preventDefault();save();}}>
   <label className="field"><span>{L('provider')}</span><select disabled={busy} value={provider} onChange={event=>chooseProvider(event.target.value)}>{presets.map(preset=><option key={preset.id} value={preset.id}>{preset.label}</option>)}</select></label>
   <label className="field"><span>{L('endpoint')}</span><input disabled={busy} value={endpoint} onChange={event=>{setEndpoint(event.target.value);invalidate();}}/></label>
   <label className="field"><span>{L('model')}</span><input disabled={busy} value={model} onChange={event=>{setModel(event.target.value);invalidate();}}/></label>
   <label className="field"><span>{L('key')}</span><input ref={keyInput} type="password" autoComplete="off" autoCapitalize="none" spellCheck={false} disabled={busy} onChange={event=>{setHasKey(event.currentTarget.value.trim().length>0);invalidate();}}/></label>
   <div className="form-row"><button type="button" disabled={busy||!canTest} onClick={testDraft}>{L('test')}</button><button type="submit" className="primary" disabled={busy||!proof||!canTest}>{L('save')}</button><button type="button" disabled={busy} onClick={cancelEditor}>{L('cancel')}</button></div>
   {connectionStatus}
  </form>}
  <section className="wf-section ai-personalization" aria-labelledby="ai-personalization-title">
   <h4 id="ai-personalization-title">{L('personalization')}</h4>
   <div className="field ai-avatar-field"><span>{L('avatar')}</span><div className="ai-avatar-controls">
    <div className="ai-avatar-preview">{personalization.avatarDataUrl?<img src={personalization.avatarDataUrl} alt={personalization.userName||L('avatar')}/>:<UserRound aria-hidden="true"/>}</div>
    <button type="button" disabled={personalizationDisabled} onClick={()=>avatarInput.current?.click()}>{L('changeAvatar')}</button>
    <button type="button" disabled={personalizationDisabled||!personalization.avatarDataUrl} onClick={()=>updatePersonalization({avatarDataUrl:null})}>{L('remove')}</button>
    <input ref={avatarInput} type="file" accept="image/jpeg,image/png,image/webp" hidden onChange={event=>{const file=event.currentTarget.files?.[0];event.currentTarget.value='';if(file)chooseAvatar(file);}}/>
   </div></div>
   <label className="field"><span>{L('userName')}</span><input disabled={personalizationDisabled} maxLength={80} value={personalization.userName} placeholder={L('userNamePlaceholder')} onChange={event=>updatePersonalization({userName:event.currentTarget.value})}/></label>
   <label className="field"><span>{L('replyLanguage')}</span><select disabled={personalizationDisabled} value={personalization.replyLanguage} onChange={event=>updatePersonalization({replyLanguage:event.currentTarget.value as AiPersonalization['replyLanguage']})}>
    <option value="app">{L('followAppLanguage')}</option><option value="en">English</option><option value="ja">日本語</option><option value="ko">한국어</option><option value="zh-Hans">简体中文</option><option value="zh-Hant">繁體中文</option>
   </select></label>
   <label className="field"><span>{L('replyPersonality')}</span><select disabled={personalizationDisabled} value={personalization.personality} onChange={event=>updatePersonalization({personality:event.currentTarget.value as AiPersonalization['personality']})}>{getAiPersonalities(locale).map(personality=><option key={personality.id} value={personality.id}>{personality.title}</option>)}</select></label>
   {personalization.personality==='custom'&&<label className="field"><span>{L('customPrompt')}</span><textarea disabled={personalizationDisabled} maxLength={4000} value={personalization.customPrompt??''} onChange={event=>updatePersonalization({customPrompt:event.currentTarget.value})}/></label>}
   <div className="form-row"><button type="button" className="primary" disabled={personalizationDisabled||!personalizationDirty||personalization.personality==='custom'&&!personalization.customPrompt?.trim()} onClick={savePersonalization}>{L('save')}</button>{result==='personalizationSaved'&&<span className="ai-personalization-saved" role="status">{L('personalizationSaved')}</span>}</div>
  </section>
  <div className="wf-section"><button type="button" disabled={disabled||editing||jobsActive||!ready||!featuresEnabled} onClick={()=>void run(async()=>{const value=await startAi({mode:'organize',productIds:[],scopes});if(mounted.current)setJobId(value.jobId);})}>{L('organizeTags')}</button>
   {jobId&&<div className="wf-progress"><span role="status">{L(job?.errorCode||job?.state||'queued')}</span>{jobActive&&(typeof job?.progress==='number'?<progress value={job.progress} max={100}/>:<progress/>)}{jobActive?<button disabled={busy} onClick={()=>void run(async()=>{await cancelAi(jobId);})}>{L('cancel')}</button>:job&&['failed','needsAction','cancelled','interrupted','paused'].includes(job.state)&&<button disabled={busy||!featuresEnabled} onClick={()=>void run(async()=>{const value=await retryAi(jobId);if(mounted.current)setJobId(value.jobId);})}>{L('retry')}</button>}</div>}
  </div>
  <div className="wf-section"><Toggle label={L('autoAI')} checked={auto} disabled={disabled||editing||(!ready&&!auto)} onChange={value=>savePrefs(scopes,value)}/><div className="ai-scopes">{(['title','tags','description','files','images'] as const).map(field=><label key={field}><input type="checkbox" checked={scopes[field]} disabled={disabled||editing||field==='images'&&!selected?.visionSupported&&!scopes.images} onChange={event=>savePrefs({...scopes,[field]:event.target.checked})}/>{L('scope-'+field)}</label>)}</div></div>
  {!connectionStatus&&(error||result&&result!=='personalizationSaved'||busy)&&<div className={'ai-status'+(error?' error':'')} role={error?'alert':'status'}>{L(error||result||(testing?'aiTesting':'running'))}</div>}
 </div>;
}
