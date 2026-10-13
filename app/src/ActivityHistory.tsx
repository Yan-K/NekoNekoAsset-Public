import {useEffect,useMemo,useState} from 'react';
import type {Account,AssetFile,Locale,Product} from './model';
import {errorCode,invokeAction,progressPercent,type NativeJob} from './backend';
import {isStoreJob,isStoreSyncJob,needsStoreSignIn,syncProgressLabel} from './Workflows';
import {nativeErrorText} from './error-i18n';
import {nativeWorkflowText} from './native-workflow-i18n';
import {uxText} from './ux-i18n';
import {t} from './i18n';
import {shellText} from './shell-i18n';
import {unityText,unityError,unityProgressLabel} from './unity-i18n';
import './ActivityHistory.css';

export interface ActivityHistoryProps {
 locale:Locale;jobs:NativeJob[];accounts:Account[];products:Product[];aiEnabled:boolean;
 onClear:()=>Promise<void>;onCancel:(job:NativeJob)=>void;onRetry:(job:NativeJob)=>void;onSignIn:(job:NativeJob)=>void;onOpenStorage:()=>void;onUnityResults:(job:NativeJob)=>void;onUnityRetry:(job:NativeJob)=>void;
}
type ActivityJob=NativeJob&{store?:string;fileIds?:string[];selectedFileIds?:string[];failures?:Record<string,unknown>};
type Detail={stamp:string;product?:Product};
const active=(job:NativeJob)=>['queued','running'].includes(job.state);
const failed=(job:NativeJob)=>['failed','needsAction'].includes(job.state);
const terminal=(job:NativeJob)=>['completed','failed','needsAction','cancelled'].includes(job.state);
const productIds=(job:NativeJob)=>[...new Set([job.productId,...(job.productIds??[])].filter((id):id is string=>typeof id==='string'&&!!id))];
const jobTime=(job:NativeJob)=>Date.parse(job.createdAt??job.updatedAt??'')||0;
const basename=(value:string)=>value.replaceAll('\\','/').split('/').at(-1)?.slice(0,512)||'';

export default function ActivityHistory({locale,jobs,accounts,products,aiEnabled,onClear,onCancel,onRetry,onSignIn,onOpenStorage,onUnityResults,onUnityRetry}:ActivityHistoryProps){
 const [limit,setLimit]=useState(200),[details,setDetails]=useState<Record<string,Detail>>({});
 const [clearing,setClearing]=useState(false),[clearError,setClearError]=useState('');
 const rows=useMemo(()=>[...jobs].sort((a,b)=>jobTime(b)-jobTime(a)||b.id.localeCompare(a.id)).slice(0,limit) as ActivityJob[],[jobs,limit]);
 const knownProducts=useMemo(()=>new Map(products.map(product=>[product.id,product])),[products]);
 const knownAccounts=useMemo(()=>new Map(accounts.map(account=>[account.id,account])),[accounts]);
 const label=(key:string)=>uxText(locale,key)??nativeWorkflowText(locale,key);
 const reason=(value:unknown,fallback='operationFailed')=>{
  const code=errorCode(value),localized=uxText(locale,code)??nativeWorkflowText(locale,code);
  return localized!==code?localized:nativeErrorText(locale,code)??label(fallback);
 };
 // Only visible jobs request metadata. A changing terminal job may replace its cached details;
 // progress ticks do not repeatedly load the library or manufacture history from Attention.
 const requestKey=useMemo(()=>{
  const wanted=new Map<string,string>();
  for(const job of rows)for(const id of productIds(job)){
   if(wanted.size>=200&&!wanted.has(id))continue;
   if(!knownProducts.has(id)||failed(job)){
    const stamp=failed(job)?`${job.id}:${job.updatedAt??job.completedAt??job.state}`:'title';
    if(!wanted.has(id))wanted.set(id,stamp);
   }
  }
  return JSON.stringify([...wanted]);
 },[rows,knownProducts]);
 useEffect(()=>{
  let stopped=false,index=0;
  const requests=JSON.parse(requestKey) as [string,string][];
  const pending=requests.filter(([id,stamp])=>details[id]?.stamp!==stamp);
  async function next(){
   while(!stopped&&index<pending.length){
    const [id,stamp]=pending[index++];let product:Product|undefined;
    try{product=await invokeAction<Product>('catalogue','product_details',{id});}catch{/* A removed product must not hide its job. */}
    if(!stopped)setDetails(current=>({...current,[id]:{stamp,product}}));
   }
  }
  for(let worker=0;worker<Math.min(4,pending.length);worker++)void next();
  return()=>{stopped=true;};
  // Cache arrivals do not cancel/restart an in-flight batch.
 },[requestKey]);
 const product=(id:string)=>details[id]?.product??knownProducts.get(id);
 const title=(id:string)=>knownProducts.get(id)?.title||product(id)?.title||t(locale,'unknown');
 const jobLabel=(job:NativeJob)=>job.type==='storage:delete_local_products'?shellText(locale,'deleteLocalProducts')!:job.type==='unity:import'?(uxText(locale,'importUnity')??nativeWorkflowText(locale,'unity')):label(job.type.startsWith('ai:')?'aiTags':isStoreJob(job.type)?(/login$/i.test(job.type)?'signIn':'sync'):job.type==='download'?'download':job.type==='storage:migrate_root'?'move':job.type==='storage:export_backup'?'export':job.type==='storage:restore_backup'||job.type==='storage:import_local'?'import':job.type==='storage:export_zip'?'exportZip':'storage');
 function failures(job:ActivityJob){
  const result:{key:string;name:string;reason:string}[]=[];
  const ids=productIds(job),files=ids.flatMap(id=>product(id)?.files??[]);
  for(const [id,value] of Object.entries(job.failures??{})){
   const item=value&&typeof value==='object'?value as Record<string,unknown>:undefined;
   const file=files.find(file=>file.id===(item?.fileId??id));
   const productId=typeof item?.productId==='string'?item.productId:ids.includes(id)?id:undefined;
   const suppliedName=item?.fileName??item?.name;
   const name=productId?title(productId):file?basename(file.name):typeof suppliedName==='string'&&!suppliedName.includes('://')?basename(suppliedName):label('fileFailure');
   result.push({key:id,name,reason:reason(item?.errorCode??value)});
  }
  if(result.length||!failed(job)||!['download','storage:import_local'].includes(job.type))return result;
  const selection=job.selectedFileIds?.length?job.selectedFileIds:job.fileIds;
  for(const file of files){
   if(selection?.length&&!selection.includes(file.id))continue;
   if(file.errorCode==='cancelled'||!file.errorCode&&!['failed','extractionFailed','missing'].includes(file.status))continue;
   const owner=ids.length>1?ids.find(id=>product(id)?.files.some(item=>item.id===file.id)):undefined;
   result.push({key:file.id,name:[owner?title(owner):'',basename(file.name)].filter(Boolean).join(' · '),reason:fileReason(file)});
  }
  return result;
 }
 function fileReason(file:AssetFile){
  return file.errorCode?reason(file.errorCode,'fileFailure'):file.status==='extractionFailed'?label('extractionFailure'):file.status==='missing'?reason('missing'):label('downloadFailure');
 }
 async function clear(){
  setClearing(true);setClearError('');
  try{await onClear();}catch(error){setClearError(reason(error));}finally{setClearing(false);}
 }
 return <section className="activity-page activity-history">
  <header className="toolbar"><h1>{t(locale,'activity')}</h1><button type="button" disabled={clearing||!jobs.some(terminal)} onClick={()=>void clear()}>{uxText(locale,'clearHistory')}</button></header>
  {clearError&&<p className="activity-history-error" role="alert">{clearError}</p>}
  {!jobs.length?<div className="empty-state">{uxText(locale,'emptyHistory')}</div>:<div className="activity-list">
   {rows.map(job=>{
    const ids=productIds(job),account=job.accountId?knownAccounts.get(job.accountId):undefined;
    const provider=account?.store||job.store||(/^booth/i.test(job.type)?'BOOTH':/^gumroad/i.test(job.type)?'Gumroad':/^jinxxy/i.test(job.type)?'Jinxxy':'');
    const names=ids.map(title),identity=job.type==='unity:import'?[job.projectName,job.unityVersion].filter(Boolean).join(' · '):isStoreJob(job.type)?[provider,account?.name||job.accountName||(job.accountId?`${t(locale,'account')} · ${t(locale,'unknown')}`:'')].filter(Boolean).join(' · '):names[0];
    const errors=failures(job),percent=progressPercent(job),date=Date.parse(job.completedAt??job.updatedAt??job.createdAt??'');
    const canRetry=job.type.startsWith('ai:')||isStoreJob(job.type)||job.type==='download'||job.type==='storage:import_local'||job.type==='storage:delete_local_products';
    const accountRemoved=isStoreJob(job.type)&&!!job.accountId&&!account;
    const accountBusy=isStoreJob(job.type)&&!!job.accountId&&jobs.some(other=>other.accountId===job.accountId&&active(other));const signInRequired=needsStoreSignIn(job,account);
    return <article className="activity-row" key={job.id}>
     <div className="activity-info">
      <strong title={identity||jobLabel(job)}>{identity||jobLabel(job)}</strong>
      <div className="activity-history-meta"><span>{jobLabel(job)} · {job.type==='unity:import'?unityProgressLabel(locale,{...job,stage:job.stage??job.state}):isStoreSyncJob(job.type)?syncProgressLabel(locale,job):label(job.state)}</span>{Number.isFinite(date)&&<time dateTime={new Date(date).toISOString()}>{new Date(date).toLocaleString(locale)}</time>}</div>
      {isStoreJob(job.type)&&names.length===1&&<small className="activity-history-reason">{names[0]}</small>}
      {names.length>1&&<details className="activity-history-products"><summary>{t(locale,'products')} · {names.length.toLocaleString(locale)}</summary><ul>{ids.map(id=><li key={id}>{title(id)}</li>)}</ul></details>}
      {active(job)&&<progress aria-label={jobLabel(job)} value={percent===undefined?undefined:Math.max(0,Math.min(100,percent))} max={100}/>}
      {job.type==='unity:import'&&job.resultCounts&&Object.values(job.resultCounts).some(Boolean)&&<small className="activity-history-reason">{(['applied','skipped','failed'] as const).filter(state=>job.resultCounts![state]>0).map(state=>`${unityText(locale,state)} ${job.resultCounts![state].toLocaleString(locale)}`).join(' · ')}</small>}
      {job.errorCode&&<small className="activity-history-reason">{job.type==='unity:import'?unityError(locale,job.errorCode):reason(job.errorCode)}</small>}
      {errors.length>0&&<div className="activity-history-failures">{errors.slice(0,3).map(item=><div key={item.key}><span>{item.name}</span><small>{item.reason}</small></div>)}{errors.length>3&&<details><summary>{t(locale,'more')} · {(errors.length-3).toLocaleString(locale)}</summary>{errors.slice(3).map(item=><div key={item.key}><span>{item.name}</span><small>{item.reason}</small></div>)}</details>}</div>}
     </div>
     <div className={job.type==='unity:import'?'activity-history-actions unity-job-actions':'activity-history-actions'}>{job.type==='unity:import'?<><button type="button" onClick={()=>onUnityResults(job)}>{unityText(locale,'results')}</button>{active(job)&&!job.writesStarted&&<button type="button" onClick={()=>onCancel(job)}>{t(locale,'cancel')}</button>}{['failed','interrupted','cancelled'].includes(job.stage??'')&&<button type="button" onClick={()=>onUnityRetry(job)}>{unityText(locale,'retry')}</button>}</>:active(job)?job.type==='storage:delete_local_products'?null:<button type="button" onClick={()=>onCancel(job)}>{t(locale,'cancel')}</button>:job.state!=='completed'&&(canRetry?<button type="button" disabled={accountRemoved||accountBusy||job.type.startsWith('ai:')&&!aiEnabled} onClick={()=>signInRequired?onSignIn(job):onRetry(job)}>{t(locale,signInRequired?'signIn':'retry')}</button>:<button type="button" onClick={onOpenStorage}>{t(locale,'storage')}</button>)}</div>
    </article>;
   })}
   {jobs.length>limit&&<button className="activity-history-more" type="button" onClick={()=>setLimit(value=>value+200)}>{t(locale,'more')} · {(jobs.length-limit).toLocaleString(locale)}</button>}
  </div>}
 </section>;
}
