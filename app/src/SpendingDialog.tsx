import {useEffect, useMemo, useRef, useState} from 'react';
import type {Locale} from './model';
import {invokeAction,nativeFileAction} from './backend';
import {isTauri} from '@tauri-apps/api/core';
import './SpendingDialog.css';

export interface SpendingTotal {currency:string;amount:string;minorUnits:string;fractionDigits:number}
export interface SpendingSummary {
 totals:SpendingTotal[];
 products:{productId:string;title:string;totals:SpendingTotal[];unknownPaymentCount:number}[];
 skippedCount:number;
}
export interface SpendingDialogProps {
 locale:Locale;productIds:string[];accountIds?:string[];onClose:()=>void;
}
// Fixed celebration tiers, roughly rounded from the JPY baseline; never convert totals.
// ECB 2026-10-09 EUR reference: JPY177.34/USD1.1206/GBP0.84763/CAD1.5942/
// AUD1.6052/CNY7.4992/HKD8.7940/KRW1503.27. CBC 2026-10-08: TWD31.875/USD.
// Remaining ECB EUR rates: CHF0.9313/NZD1.9961/SGD1.4348/SEK11.1675/NOK10.7155/
// DKK7.4751/PLN4.3835/CZK24.369/HUF365.08/RON5.3406/BRL5.6086/INR108.3965/
// IDR20036.94/MYR4.5777/PHP70.397/THB37.574/ZAR18.5270/MXN20.3950/
// TRY55.1033/ILS3.4260/ISK136.80. Each rounded base approximates rate/177.34*1000.
// https://www.ecb.europa.eu/stats/policy_and_exchange_rates/euro_reference_exchange_rates/html/index.en.html
// https://www.cbc.gov.tw/en/lp-700-2-1-40.html
const FIREWORK_THRESHOLDS:Record<string,number[]>={
 JPY:[1000,10000,100000,500000,1000000],
 USD:[6,60,600,3000,6000],EUR:[6,60,600,3000,6000],GBP:[5,50,500,2500,5000],
 CAD:[9,90,900,4500,9000],AUD:[9,90,900,4500,9000],
 KRW:[8500,85000,850000,4250000,8500000],TWD:[200,2000,20000,100000,200000],
 CNY:[40,400,4000,20000,40000],HKD:[50,500,5000,25000,50000],
 CHF:[5,50,500,2500,5000],NZD:[11,110,1100,5500,11000],SGD:[8,80,800,4000,8000],
 SEK:[60,600,6000,30000,60000],NOK:[60,600,6000,30000,60000],DKK:[40,400,4000,20000,40000],
 PLN:[25,250,2500,12500,25000],CZK:[140,1400,14000,70000,140000],HUF:[2000,20000,200000,1000000,2000000],
 RON:[30,300,3000,15000,30000],BRL:[30,300,3000,15000,30000],INR:[600,6000,60000,300000,600000],
 IDR:[110000,1100000,11000000,55000000,110000000],MYR:[25,250,2500,12500,25000],PHP:[400,4000,40000,200000,400000],
 THB:[210,2100,21000,105000,210000],ZAR:[100,1000,10000,50000,100000],MXN:[120,1200,12000,60000,120000],
 TRY:[300,3000,30000,150000,300000],ILS:[20,200,2000,10000,20000],ISK:[800,8000,80000,400000,800000],
};
export function fireworkTier(totals:SpendingTotal[]):number {
 let tier=0;
 for(const total of totals){
  if(!/^-?\d+$/.test(total.minorUnits)||!Number.isInteger(total.fractionDigits)||total.fractionDigits<0||total.fractionDigits>6)continue;
  const levels=FIREWORK_THRESHOLDS[total.currency];
  if(!levels)continue;
  const amount=BigInt(total.minorUnits);
  const scale=10n**BigInt(total.fractionDigits);
  for(let index=0;index<levels.length;index++)if(amount>=BigInt(levels[index])*scale)tier=Math.max(tier,index+1);
 }
 return tier;
}
export function formatSpendingTotal(total:SpendingTotal,locale:Locale):string {
 if(!/^-?\d+$/.test(total.minorUnits)||!Number.isInteger(total.fractionDigits)||total.fractionDigits<0||total.fractionDigits>6)return `${total.currency} —`;
 const raw=BigInt(total.minorUnits),negative=raw<0n,absolute=negative?-raw:raw;
 const scale=10n**BigInt(total.fractionDigits),whole=absolute/scale;
 const grouped=new Intl.NumberFormat(locale,{maximumFractionDigits:0}).format(whole);
 const decimal=new Intl.NumberFormat(locale).formatToParts(1.1).find(part=>part.type==='decimal')?.value??'.';
 const fraction=total.fractionDigits?decimal+(absolute%scale).toString().padStart(total.fractionDigits,'0'):'';
 return `${total.currency} ${negative?'−':''}${grouped}${fraction}`;
}
const strings={
 en:{title:'Spending summary',loading:'Loading…',empty:'No known payments',products:'Products',skipped:'Skipped: {count}',unknown:'Unknown payments: {count}',share:'Share result',copied:'Copied',copyFailed:'Could not copy',close:'Close',failed:'Could not load spending summary',retry:'Retry',phrase:'My asset spending'},
 ja:{title:'金額集計',loading:'読み込み中…',empty:'確認できる支払額はありません',products:'商品',skipped:'集計対象外：{count}',unknown:'不明な支払額：{count}',share:'結果を共有',copied:'コピーしました',copyFailed:'コピーできませんでした',close:'閉じる',failed:'購入金額を読み込めませんでした',retry:'再試行',phrase:'アセット購入金額'},
 ko:{title:'구매 금액 합계',loading:'불러오는 중…',empty:'확인된 결제 금액이 없습니다',products:'상품',skipped:'집계 제외: {count}',unknown:'알 수 없는 결제: {count}',share:'결과 공유',copied:'복사됨',copyFailed:'복사하지 못했습니다',close:'닫기',failed:'구매 금액을 불러오지 못했습니다',retry:'다시 시도',phrase:'내 에셋 구매 금액'},
 'zh-Hans':{title:'消费汇总',loading:'正在加载…',empty:'没有已知付款',products:'商品',skipped:'已跳过：{count}',unknown:'未知付款：{count}',share:'分享结果',copied:'已复制',copyFailed:'无法复制',close:'关闭',failed:'无法加载消费汇总',retry:'重试',phrase:'我的素材消费'},
 'zh-Hant':{title:'消費彙總',loading:'載入中…',empty:'沒有已知付款',products:'商品',skipped:'已略過：{count}',unknown:'未知付款：{count}',share:'分享結果',copied:'已複製',copyFailed:'無法複製',close:'關閉',failed:'無法載入消費彙總',retry:'重試',phrase:'我的素材消費'},
} satisfies Record<Locale,Record<string,string>>;

type Particle={x:number;y:number;vx:number;vy:number;life:number;maxLife:number;hue:number};
function Fireworks({tier}:{tier:number}) {
 const canvas=useRef<HTMLCanvasElement>(null);
 useEffect(()=>{
  if(!tier)return;
  const surface=canvas.current,context=surface?.getContext('2d');
  if(!surface||!context)return;
  const motion=window.matchMedia('(prefers-reduced-motion: reduce)');
  let frame=0,width=0,height=0,particles:Particle[]=[],previous=0,start=0,nextBurst=0;
  const resize=()=>{width=window.innerWidth;height=window.innerHeight;const ratio=Math.min(window.devicePixelRatio||1,2);surface.width=Math.ceil(width*ratio);surface.height=Math.ceil(height*ratio);context.setTransform(ratio,0,0,ratio,0,0);};
  const stop=()=>{cancelAnimationFrame(frame);frame=0;particles=[];context.clearRect(0,0,width,height);};
  const burst=()=>{
   const x=width*(.12+Math.random()*.76),y=height*(.12+Math.random()*(tier===5?.7:.5)),hue=Math.random()*360;
   const count=[0,22,34,46,66,100][tier],spread=[0,45,75,105,145,220][tier];
   for(let index=0;index<count&&particles.length<520;index++){
    const angle=Math.random()*Math.PI*2,speed=35+Math.random()*spread,life=.7+Math.random()*.8;
    particles.push({x,y,vx:Math.cos(angle)*speed,vy:Math.sin(angle)*speed,life,maxLife:life,hue});
   }
  };
  const draw=(time:number)=>{
   if(motion.matches){stop();return;}
   if(!start)start=time;
   const dt=Math.min(previous?(time-previous)/1000:0,.05);previous=time;
   context.clearRect(0,0,width,height);
   const duration=tier===5?Infinity:1200+tier*850;
   if(time-start<duration&&time>=nextBurst){burst();nextBurst=time+(tier===5?220:900-tier*120);}
   particles=particles.filter(particle=>particle.life>0);
   for(const particle of particles){
    particle.life-=dt;particle.x+=particle.vx*dt;particle.y+=particle.vy*dt;particle.vy+=55*dt;
    context.globalAlpha=Math.max(0,particle.life/particle.maxLife)*.85;
    context.fillStyle=`hsl(${particle.hue} 80% 65%)`;context.beginPath();context.arc(particle.x,particle.y,1.1+tier*.3,0,Math.PI*2);context.fill();
   }
   context.globalAlpha=1;
   if(time-start<duration||particles.length)frame=requestAnimationFrame(draw);else frame=0;
  };
  const updateMotion=()=>{stop();if(!motion.matches){previous=0;start=0;nextBurst=0;frame=requestAnimationFrame(draw);}};
  resize();window.addEventListener('resize',resize);motion.addEventListener('change',updateMotion);updateMotion();
  return()=>{stop();window.removeEventListener('resize',resize);motion.removeEventListener('change',updateMotion);};
 },[tier]);
 return <canvas ref={canvas} className="spending-fireworks" aria-hidden="true"/>;
}

export default function SpendingDialog({locale,productIds,accountIds,onClose}:SpendingDialogProps) {
 const [summary,setSummary]=useState<SpendingSummary|null>(null),[failed,setFailed]=useState(false),[retry,setRetry]=useState(0),[shareState,setShareState]=useState<'copied'|'copyFailed'|''>('');
 const box=useRef<HTMLDivElement>(null),close=useRef(onClose);close.current=onClose;
 const L=strings[locale],selection=JSON.stringify({productIds,accountIds});
 useEffect(()=>{
  let active=true;setSummary(null);setFailed(false);setShareState('');
  const args=JSON.parse(selection) as {productIds:string[];accountIds?:string[]};
  void invokeAction<SpendingSummary>('catalogue','spending_summary',args).then(result=>{if(active)setSummary(result);},()=>{if(active)setFailed(true);});
  return()=>{active=false;};
 },[selection,retry]);
 useEffect(()=>{
  const previous=document.activeElement as HTMLElement|null;
  box.current?.querySelector<HTMLButtonElement>('button')?.focus();
  const key=(event:KeyboardEvent)=>{
   if(event.isComposing||event.keyCode===229)return;
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close.current();}
   if(event.key==='Tab'){
    const nodes=Array.from(box.current?.querySelectorAll<HTMLElement>('button:not(:disabled),[tabindex="0"]')??[]).filter(node=>node.getClientRects().length);
    const first=nodes[0],last=nodes.at(-1);
    if(event.shiftKey&&(document.activeElement===first||!nodes.includes(document.activeElement as HTMLElement))){event.preventDefault();last?.focus();}
    else if(!event.shiftKey&&(document.activeElement===last||!nodes.includes(document.activeElement as HTMLElement))){event.preventDefault();first?.focus();}
   }
  };
  document.addEventListener('keydown',key,true);
  return()=>{document.removeEventListener('keydown',key,true);if(previous?.isConnected)previous.focus();};
 },[]);
 const tier=useMemo(()=>fireworkTier(summary?.totals??[]),[summary]);
 const unknown=summary?.products.reduce((count,product)=>count+product.unknownPaymentCount,0)??0;
 const count=(text:string,value:number)=>text.replace('{count}',value.toLocaleString(locale));
 const share=async()=>{
  if(!summary?.totals.length)return;
  const text=[L.phrase,...summary.totals.map(total=>formatSpendingTotal(total,locale)),'#NekoNekoAsset'].join('\n');
  setShareState('');
  try{
   if(isTauri()){
    const result=await nativeFileAction<{copied:boolean}>('copy_text',{text});
    if(result.copied!==true)throw new Error('clipboard_unavailable');
   }else await navigator.clipboard.writeText(text);
   setShareState('copied');
  }catch{setShareState('copyFailed');}
 };
 return <div className="modal-backdrop spending-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget)onClose();}}>
  <Fireworks tier={tier}/>
  <div className="modal spending-dialog" ref={box} role="dialog" aria-modal="true" aria-labelledby="spending-title" aria-busy={!summary&&!failed}>
   <header className="modal-header"><h2 id="spending-title">{L.title}</h2><button type="button" aria-label={L.close} onClick={onClose}>×</button></header>
   <div className="modal-body spending-body">
    {!summary&&!failed&&<div role="status">{L.loading}</div>}
    {failed&&<div className="spending-error" role="alert"><span>{L.failed}</span><button type="button" onClick={()=>setRetry(value=>value+1)}>{L.retry}</button></div>}
    {summary&&<>
     <div className={'spending-totals'+(tier===5?' spending-totals-celebration':'')}>
      {summary.totals.length?summary.totals.map(total=><strong key={total.currency}>{formatSpendingTotal(total,locale)}</strong>):<span>{L.empty}</span>}
     </div>
     {(summary.skippedCount>0||unknown>0)&&<div className="spending-counts">{summary.skippedCount>0&&<span>{count(L.skipped,summary.skippedCount)}</span>}{unknown>0&&<span>{count(L.unknown,unknown)}</span>}</div>}
     {summary.products.length>0&&<section className="spending-products" aria-label={L.products}><h3>{L.products}</h3><ul>{summary.products.map(product=><li key={product.productId}><span>{product.title}</span><div>{product.totals.length?product.totals.map(total=><span key={total.currency}>{formatSpendingTotal(total,locale)}</span>):<span>—</span>}</div></li>)}</ul></section>}
    </>}
   </div>
   <footer className="modal-footer"><span className="spending-share-status" role="status">{shareState?L[shareState]:''}</span><button type="button" disabled={!summary?.totals.length} onClick={()=>void share()}>{L.share}</button><button type="button" onClick={onClose}>{L.close}</button></footer>
  </div>
 </div>;
}
