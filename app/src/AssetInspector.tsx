import {useEffect,useId,useLayoutEffect,useRef,useState,type ReactNode} from 'react';
import {FileArchive,FileBox,RefreshCw,Download,Plus,SlidersHorizontal,FolderOpen,ExternalLink,Trash2,Check,X,ChevronDown} from 'lucide-react';
import type {Product,Locale,Account} from './model';
import {getProductDisplayStatus,getEffectiveTags,getEffectiveAvatarTags,sourceCategoryLabel,isFileDownloaded,isProductDownloaded,priceLabel,currentPriceLabel,fileCountLabel} from './model';
import {t} from './i18n';
import {shellText} from './shell-i18n';
import {wf} from './workflow-i18n';
import Preview from './Preview';
import {StatusIcon} from './LibraryIcons';
import './inspector.css';
const fileActionLabels: Record<Locale,{openFolder:string;openFile:string;openPage:string}> = {
 en:{openFolder:'Open folder',openFile:'Open file',openPage:'Open product page'},ja:{openFolder:'フォルダーを開く',openFile:'ファイルを開く',openPage:'商品ページを開く'},ko:{openFolder:'폴더 열기',openFile:'파일 열기',openPage:'상품 페이지 열기'},'zh-Hans':{openFolder:'打开文件夹',openFile:'打开文件',openPage:'打开商品页面'},'zh-Hant':{openFolder:'開啟資料夾',openFile:'開啟檔案',openPage:'開啟商品頁面'}
};
function isZip(file:{name:string;type:string}):boolean {
 const extension=file.name.trim().match(/\.([^./\\]+)$/)?.[1].toLowerCase();
 return extension!==undefined?extension==='zip':['zip','application/zip'].includes(file.type.trim().toLowerCase());
}
/** Only checks retained original ZIP facts; extracted-file actions use native availability. */
export function canOpenZip(file:Product['files'][number]):boolean {
 return isZip(file)&&isFileDownloaded(file)&&file.originalAvailable===true&&!!file.verifiedHash&&!!file.rootId&&!!file.relativePath;
}
export function canOpenProductPage(product:Pick<Product,'sourceUrl'|'sourceUrlAvailable'>):boolean {
 if(product.sourceUrlAvailable===false||!product.sourceUrl)return false;
 const value=product.sourceUrl.trim();if(!/^https?:\/\//i.test(value)||/[\u0000-\u0020\u007f]/.test(value))return false;
 try{const url=new URL(value);return !!url.hostname&&!url.username&&!url.password&&['http:','https:'].includes(url.protocol);}catch{return false;}
}
function sourceLabel(locale:Locale,value:string|undefined|null):string {
 const clean=value?.trim();if(!clean||/^(unknown|null|undefined)$/i.test(clean))return '';
 const keys:Record<string,string>={library:'library',purchase:'orders',purchases:'orders',purchased:'orders',order:'orders',orders:'orders',gift:'gifts',gifts:'gifts',free:'free',local:'localFiles','local files':'localFiles','offline files':'localFiles'};
 const key=keys[clean.toLowerCase()];return key?wf(locale,key):clean.toLowerCase()==='booth'?'BOOTH':clean;
}
export function provenanceLines(locale:Locale,rows:readonly {source?:string;accountId?:string;acquisitionKind?:string|null;acquiredDateText?:string|null;variantName?:string|null}[],accounts:readonly Account[]):string[] {
 return [...new Set(rows.map(row=>{
  const account=accounts.find(account=>account.id===row.accountId);
  return [...new Set([account?.store,account?.name,sourceLabel(locale,row.source),sourceLabel(locale,row.acquisitionKind),row.acquiredDateText,row.variantName].map(value=>value?.trim()).filter((value):value is string=>!!value&&!/^(unknown|null|undefined)$/i.test(value)))].join(' · ');
 }).filter(Boolean))];
}

export interface AssetInspectorProps {
 product:Product;locale:Locale;wide:boolean;tab:'details'|'files';onTabChange:(tab:'details'|'files')=>void;
 sensitiveMode?:string;revealed?:boolean;onReveal:()=>void;accountIds?:string[]|null;accounts?:Account[];
 actions:ReactNode;onEdit:()=>void;onTagClick?:(tag:string)=>void;onFileRetry?:(fileId:string,redownload:boolean)=>void;retryDisabled?:boolean;busyFileIds?:readonly string[];
 onFileOpen?:(fileId:string)=>void;onOpenFolder?:()=>void;folderAvailable?:boolean;openableFileIds?:readonly string[];
 onOpenProductPage?:(url:string)=>void;
 onDownloadAll?:()=>void;
 onLocalFilesAdd?:(folder:boolean)=>void;
 onLocalFileRemove?:(fileId:string)=>void;
 localFilesBusy?:boolean;
}
export default function AssetInspector({product:p,locale,wide,tab,onTabChange,sensitiveMode='show',revealed=false,onReveal,accountIds=null,accounts=[],actions,onEdit,onTagClick,onFileRetry,retryDisabled=false,busyFileIds=[],onFileOpen,onOpenFolder,folderAvailable=false,openableFileIds=[],onOpenProductPage,onDownloadAll,onLocalFilesAdd,onLocalFileRemove,localFilesBusy=false}:AssetInspectorProps){
 const L=(key:string)=>shellText(locale,key)??t(locale,key);
 const detailScroll=useRef<HTMLDivElement>(null),fileScroll=useRef<HTMLDivElement>(null);
 const [removingFile,setRemovingFile]=useState<string|null>(null);
 useEffect(()=>{setRemovingFile(null);},[p.id]);
 useEffect(()=>{detailScroll.current?.scrollTo(0,0);fileScroll.current?.scrollTo(0,0);},[p.id]);
 // Tags show three rows and fold the rest behind "Show all". Rows are read from layout,
 // so a long tag that wraps still counts as one row and the clamp never cuts a tag.
 const tags=getEffectiveTags(p),tagKey=JSON.stringify([p.id,tags]),tagsId=useId();
 const [tagList,setTagList]=useState<HTMLDivElement|null>(null);
 const [tagRows,setTagRows]=useState<{key:string;clamp:number;full:number}|null>(null),[expandedTags,setExpandedTags]=useState('');
 useLayoutEffect(()=>{
  if(!tagList)return;
  const measure=()=>{
   let rows=0,left=Infinity,clamp=0;
   for(const item of Array.from(tagList.children) as HTMLElement[]){if(item.offsetLeft<=left)rows++;left=item.offsetLeft;if(rows<=3)clamp=Math.max(clamp,item.offsetTop+item.offsetHeight);}
   const full=tagList.offsetHeight;
   setTagRows(old=>rows<=3?null:old?.key===tagKey&&old.clamp===clamp&&old.full===full?old:{key:tagKey,clamp,full});
  };
  measure();
  const observer=new ResizeObserver(measure);observer.observe(tagList);
  return()=>observer.disconnect();
 },[tagList,tagKey]);
 const tagClamp=tagRows?.key===tagKey?tagRows:null,tagsExpanded=!!tagClamp&&expandedTags===p.id;
 const status=getProductDisplayStatus(p),owned=getEffectiveAvatarTags(p,accountIds),image=p.thumbnailHidden?undefined:p.thumbnailOverrideUrl??p.previewUrl;
 const blocked=p.sensitive&&sensitiveMode==='blur'&&!revealed;
 const pageAvailable=canOpenProductPage(p)&&!!onOpenProductPage;
 const sourceAccounts=[...accounts,...(p.sources??[]).filter(source=>source.accountId&&source.accountName&&!accounts.some(account=>account.id===source.accountId)).map(source=>({id:source.accountId!,name:source.accountName!,store:source.provider,status:''}))];
 const sourceSummaries=(p.sources??[]).map(source=>{
  const account=sourceAccounts.find(account=>account.id===source.accountId);
  return sourceLabel(locale,source.label)||[...new Set([source.provider,source.accountName??account?.name].filter(Boolean))].join(' · ');
 }).filter(Boolean);
 const productSources=[...new Set([...sourceSummaries,...provenanceLines(locale,p.provenance??[],sourceAccounts)])];
 const sourceCategory=p.sourceCategory?sourceCategoryLabel(p.sourceCategory):'';
 const accountSources=productSources.length?[]:accounts.filter(account=>p.accountIds?.includes(account.id)).map(account=>[account.store,account.name].filter(Boolean).join(' · '));
 const openPage=()=>{if(canOpenProductPage(p))onOpenProductPage?.(p.sourceUrl!.trim());};
 const currentPrices=(p.ownedCurrentPrices??[]).filter(row=>accountIds===null||row.accountIds.some(id=>accountIds.includes(id)));
 const paidPrices=(p.ownedPaidPrices??[]).filter(row=>accountIds===null||!!row.accountId&&accountIds.includes(row.accountId));
 const variantPrices=(rows:readonly {name:string;price:number|null;currency:string|null}[],fallback:string|null)=>rows.length?<span className="asset-inspector-prices">{rows.map((row,index)=><span key={index}>{row.name&&<strong>{row.name} · </strong>}{priceLabel(locale,row.price,row.currency)||L('unknown')}</span>)}</span>:<span>{fallback||L('unknown')}</span>;
 const details=<div className="asset-inspector-scroll" ref={detailScroll}>
  {image&&!blocked&&<div className="asset-inspector-ambient" aria-hidden="true"><Preview key={image} src={image} title=""/></div>}
  <div className="asset-inspector-preview">{blocked?<button type="button" className="asset-inspector-reveal" onClick={onReveal}>{L('showR18')}</button>:pageAvailable?<button type="button" className="asset-inspector-page-link" aria-label={fileActionLabels[locale].openPage} title={fileActionLabels[locale].openPage} onClick={openPage}><Preview src={image} title={p.title}/><span className="asset-inspector-page-hint" aria-hidden="true"><ExternalLink size={14}/><span>{fileActionLabels[locale].openPage}</span></span></button>:<Preview src={image} title={p.title}/>}</div>
  <div className="asset-inspector-body">
   <h2 title={p.title}>{p.title}</h2><div className="asset-inspector-shop">{p.provider==='local'?L('localProductShop'):p.shop}{p.custom&&<span className="badge">{L('custom')}</span>}</div>
   <div className="asset-inspector-status">
    <span className={'asset-inspector-state '+(status==='ready'?'positive':status==='attention'?'warning':'')} data-status={status}><StatusIcon status={status}/><span>{L(status)}</span></span><span className="asset-inspector-count" title={p.manifestComplete===true?undefined:L('totalUnverified')}>{fileCountLabel(p)}</span>
   </div>
   {actions}
   <section className="asset-inspector-section"><div className="asset-inspector-label">{L('tags')}</div><div className={'asset-inspector-tags-frame'+(tagClamp?' clamped':'')} style={tagClamp?{maxHeight:tagsExpanded?tagClamp.full:tagClamp.clamp}:undefined} onFocus={e=>{const item=e.target as HTMLElement;if(tagClamp&&!tagsExpanded&&item.offsetTop+item.offsetHeight>tagClamp.clamp)setExpandedTags(p.id);}}><div className="asset-inspector-tags" id={tagsId} ref={setTagList}>{tags.map(tag=><button type="button" key={tag} title={tag} onClick={()=>onTagClick?.(tag)}>{tag}</button>)}<button type="button" className="asset-inspector-tag-add" aria-label={L('edit')+' '+L('tags')} title={L('edit')+' '+L('tags')} onClick={onEdit}><Plus size={12}/></button></div></div>{tagClamp&&<button type="button" className="asset-inspector-tags-toggle" aria-expanded={tagsExpanded} aria-controls={tagsId} onClick={()=>setExpandedTags(tagsExpanded?'':p.id)}><span>{L(tagsExpanded?'showFewerTags':'showAllTags')}</span><ChevronDown size={13} aria-hidden="true"/></button>}</section>
   <section className="asset-inspector-section"><div className="asset-inspector-label">{L('note')}<button type="button" aria-label={L('edit')+' '+L('note')} title={L('edit')+' '+L('note')} onClick={onEdit}><SlidersHorizontal size={12}/></button></div><p className="asset-inspector-note">{p.note||'—'}</p></section>
   <section className="asset-inspector-section asset-inspector-metadata"><div><span>{L('category')}</span><span>{L(p.category)}</span></div><div><span>{L('acquired')}</span><span>{p.acquired||L('unknown')}</span></div><div><span>{L('currentPrice')}</span>{variantPrices(currentPrices.some(row=>priceLabel(locale,row.price,row.currency))?currentPrices:[],currentPriceLabel(locale,{...p,ownedCurrentPrices:currentPrices}))}</div><div><span>{L('paidPrice')}</span>{variantPrices(paidPrices,priceLabel(locale,p.paidPrice,p.paidCurrency))}</div></section>
   <details className="asset-inspector-section asset-inspector-compatibility"><summary>{L('compatibility')}</summary><div className="asset-inspector-metadata"><div><span>{L('yourFiles')}</span><strong>{owned.join(' · ')||L('unknown')}</strong></div><div><span>{L('listing')}</span><span>{p.listingAvatars.join(' · ')||L('unknown')}</span></div><div><span>{L('localEvidence')}</span><span>{L('unknown')}</span></div></div></details>
   {(sourceCategory||productSources.length>0||accountSources.length>0)&&<details className="asset-inspector-section asset-inspector-source"><summary>{L('sourceDetails')}</summary>{sourceCategory&&<p>{sourceCategory}</p>}{[...productSources,...accountSources].map((line,index)=><p key={index}>{line}</p>)}</details>}
  </div>
 </div>;
 const files=<div className="asset-inspector-files" ref={fileScroll}><div className="asset-inspector-file-heading"><strong>{L('files')}</strong><span title={p.manifestComplete===true?undefined:L('totalUnverified')}>{fileCountLabel(p)}</span><div className="asset-inspector-file-actions">{p.provider==='local'&&onLocalFilesAdd&&<><button type="button" title={L('addLocalFiles')} aria-label={L('addLocalFiles')} disabled={localFilesBusy} onClick={()=>onLocalFilesAdd(false)}><Plus size={14}/><span>{L('addLocalFiles')}</span></button><button type="button" title={L('addLocalFolder')} aria-label={L('addLocalFolder')} disabled={localFilesBusy} onClick={()=>onLocalFilesAdd(true)}><FolderOpen size={14}/></button></>}{onDownloadAll&&p.provider!=='local'&&!isProductDownloaded(p)&&<button type="button" title={L('downloadAll')} aria-label={L('downloadAll')} disabled={retryDisabled||!p.files.length} onClick={onDownloadAll}><Download size={14}/><span>{L('downloadAll')}</span></button>}<button type="button" className="asset-inspector-open-folder" aria-label={fileActionLabels[locale].openFolder} title={fileActionLabels[locale].openFolder} disabled={folderAvailable!==true||!onOpenFolder} onClick={()=>{if(folderAvailable===true)onOpenFolder?.();}}><FolderOpen size={14}/><span>{fileActionLabels[locale].openFolder}</span></button></div></div>{p.files.map(file=>{
  const openable=!!onFileOpen&&openableFileIds.includes(file.id),sources=provenanceLines(locale,file.provenance??[],sourceAccounts);
  return <div className="asset-inspector-file" key={file.id}><span className="asset-inspector-file-icon">{file.type.toLowerCase()==='unitypackage'?<FileBox size={17}/>:<FileArchive size={17}/>}</span><div className="asset-inspector-file-meta">{openable?<button type="button" className="asset-inspector-file-name" aria-label={fileActionLabels[locale].openFile+' '+file.name} title={fileActionLabels[locale].openFile} onClick={()=>{if(openableFileIds.includes(file.id))onFileOpen?.(file.id);}}>{file.name}</button>:<span title={file.name}>{file.name}</span>}<small>{[file.size,L(file.status)].filter(Boolean).join(' · ')}</small>{['failed','extractionFailed','missing'].includes(file.status)&&<small className="asset-inspector-file-error">{L(file.status==='extractionFailed'?'failedExtraction':file.status==='failed'?'failedTransfer':'fileMissing')}</small>}{sources.length>0&&<details><summary>{L('sourceDetails')}</summary>{sources.map((line,index)=><small key={index}>{line}</small>)}</details>}</div>{onFileRetry&&p.provider!=='local'&&<button type="button" aria-label={L(isFileDownloaded(file)?'redownload':'download')+' '+file.name} title={L(isFileDownloaded(file)?'redownload':'download')} disabled={busyFileIds.includes(file.id)} onClick={()=>onFileRetry(file.id,isFileDownloaded(file))}>{isFileDownloaded(file)?<RefreshCw size={13}/>:<Download size={13}/>}</button>}{p.provider==='local'&&onLocalFileRemove&&<span className="asset-inspector-local-remove">{removingFile===file.id?<><button type="button" title={L('confirmRemoveLocalFile')} aria-label={L('confirmRemoveLocalFile')+' '+file.name} disabled={localFilesBusy} onClick={()=>{onLocalFileRemove(file.id);setRemovingFile(null);}}><Check size={13}/></button><button type="button" title={L('cancel')} aria-label={L('cancel')} disabled={localFilesBusy} onClick={()=>setRemovingFile(null)}><X size={13}/></button></>:<button type="button" title={L('removeLocalFile')} aria-label={L('removeLocalFile')+' '+file.name} disabled={localFilesBusy} onClick={()=>setRemovingFile(file.id)}><Trash2 size={13}/></button>}</span>}</div>;
 })}</div>;
 return <div className={'asset-inspector'+(wide?' asset-inspector-wide':'')}>
  {!wide&&<div className="asset-inspector-tabs" role="tablist"><button type="button" role="tab" aria-selected={tab==='details'} onClick={()=>onTabChange('details')}>{L('details')}</button><button type="button" role="tab" aria-selected={tab==='files'} onClick={()=>onTabChange('files')}>{L('files')} <span title={p.manifestComplete===true?undefined:L('totalUnverified')}>{fileCountLabel(p,false)}</span></button></div>}
  <div className="asset-inspector-content">{(wide||tab==='details')&&details}{(wide||tab==='files')&&files}</div>
 </div>;
}
