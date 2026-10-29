import {useEffect,useRef,useState} from 'react';
import type {CSSProperties} from 'react';

/** Keep the slider stationary until the pointer is released. */
export default function ScaleControl({label,value,onCommit}:{label:string;value:number;onCommit:(value:number)=>void}) {
 const [draft,setDraft]=useState(value),latestDraft=useRef(value),pointer=useRef<number|null>(null);
 const currentValue=useRef(value),currentCommit=useRef(onCommit);currentValue.current=value;currentCommit.current=onCommit;
 const clamp=(next:number)=>Math.min(300,Math.max(80,next));
 const pointerStep=(next:number)=>clamp(Math.round(next/10)*10);
 const updateDraft=(next:number)=>{latestDraft.current=next;setDraft(next);};
 const commit=(next:number)=>{updateDraft(next);if(next!==currentValue.current)currentCommit.current(next);};
 useEffect(()=>{if(pointer.current===null)updateDraft(value);},[value]);
 useEffect(()=>{
  const release=(event:PointerEvent)=>{if(pointer.current!==event.pointerId)return;pointer.current=null;commit(latestDraft.current);};
  const cancel=(event:PointerEvent)=>{if(pointer.current!==event.pointerId)return;pointer.current=null;updateDraft(currentValue.current);};
  // Native range controls may change capture while dragging. Capture loss is
  // not a release; the window sees the real release, including outside input.
  window.addEventListener('pointerup',release,true);window.addEventListener('pointercancel',cancel,true);
  return()=>{pointer.current=null;window.removeEventListener('pointerup',release,true);window.removeEventListener('pointercancel',cancel,true);};
 },[]);
 return <div className="ui-scale-control"><input aria-label={label} type="range" min={80} max={300} step={1} value={draft} aria-valuetext={`${draft}%`} style={{'--fill':`${(draft-80)/220*100}%`} as CSSProperties}
  onKeyDown={event=>{if(event.altKey||event.ctrlKey||event.metaKey)return;const delta=event.key==='ArrowLeft'?-1:event.key==='ArrowRight'?1:event.key==='ArrowDown'?-10:event.key==='ArrowUp'?10:0;if(!delta)return;event.preventDefault();event.stopPropagation();if(pointer.current===null)commit(clamp(latestDraft.current+delta));}}
  onPointerDown={event=>{if(event.button===0)pointer.current=event.pointerId;}}
  onChange={event=>{const next=pointerStep(Number(event.target.value));updateDraft(next);if(pointer.current===null)commit(next);}}
 /><output>{draft}%</output></div>;
}
