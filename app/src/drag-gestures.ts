import {useEffect,useRef} from 'react';
import type {PointerEvent} from 'react';

/** Appearance › One-handed drag: a press on a selected product drags the selection onto a sidebar category;
 * a press anywhere else in the results draws a selection box. Ctrl/Shift add the box to the selection. */
export type DragGestures={selected:ReadonlySet<string>;select:(ids:string[],anchor?:string)=>void;drop:(ids:string[],category:string)=>void};
type Geometry={list:boolean;gap:number;columns:number;actual:number;row:number};
type Box={left:number;top:number;right:number;bottom:number};
const threshold=4,edge=32,inset=12;

/** Item indexes whose virtual cells meet a box in content coordinates, so rows outside the rendered window count too. */
function boxHits(box:Box,{list,gap,columns,actual,row}:Geometry,width:number,count:number):number[]{
 const cellWidth=list?width-inset*2:actual,cellHeight=list?row:row-gap,hits:number[]=[];
 for(let r=Math.max(0,Math.floor(box.top/row)),last=Math.min(Math.ceil(count/columns)-1,Math.floor(box.bottom/row));r<=last;r++){
  if(box.top>r*row+cellHeight)continue;
  for(let c=0;c<columns;c++){const left=inset+c*(actual+gap),index=r*columns+c;if(index<count&&box.right>=left&&box.left<=left+cellWidth)hits.push(index);}
 }
 return hits;
}

export function useDragGestures(geometry:Geometry,items:readonly {id:string}[],gestures:DragGestures|null){
 const latest=useRef({geometry,items,gestures}),stop=useRef<(()=>void)|null>(null);latest.current={geometry,items,gestures};
 useEffect(()=>()=>stop.current?.(),[]);
 return (e:PointerEvent<HTMLDivElement>)=>{
  const g=latest.current.gestures,viewport=e.currentTarget,content=viewport.firstElementChild as HTMLElement|null,app=viewport.closest('.app'),pressed=e.target as Element;
  if(!g||!content||!app||stop.current||e.button!==0||e.pointerType==='touch'||pressed.closest('input,button,a,select,textarea,summary,[contenteditable="true"]'))return;
  const view=viewport.getBoundingClientRect(),right=view.left+viewport.clientLeft+viewport.clientWidth,bottom=view.top+viewport.clientTop+viewport.clientHeight;
  if(e.clientX>=right||e.clientY>=bottom)return;
  const id=pressed.closest<HTMLElement>('[data-product-id]')?.dataset.productId,moving=!!id&&g.selected.has(id),before=[...g.selected];
  const base=e.ctrlKey||e.metaKey||e.shiftKey?before:[],ids=moving?before:[],pointerId=e.pointerId,root=document.documentElement;
  const local=(x:number,y:number)=>{const r=content.getBoundingClientRect();return {x:x-r.left,y:y-r.top};};
  const start=local(e.clientX,e.clientY),down={x:e.clientX,y:e.clientY};
  let pointer=down,active=false,frame=0,shown:string|null=null,overlay:HTMLDivElement|null=null,target:HTMLElement|null=null;
  const mark=(next:HTMLElement|null)=>{if(next===target)return;target?.classList.remove('drop-target');next?.classList.add('drop-target');target=next;};
  // Follows the pointer without scrolling, so the release itself never scrolls.
  const paint=()=>{
   if(!overlay)return;
   if(moving){overlay.style.transform=`translate(${pointer.x+12}px,${pointer.y+14}px)`;mark(document.elementFromPoint(pointer.x,pointer.y)?.closest<HTMLElement>('[data-drop-category]')??null);return;}
   const now=local(pointer.x,pointer.y),r=content.getBoundingClientRect();
   const box={left:Math.min(start.x,now.x),top:Math.min(start.y,now.y),right:Math.max(start.x,now.x),bottom:Math.max(start.y,now.y)};
   const left=Math.max(view.left,r.left+box.left),top=Math.max(view.top,r.top+box.top);
   Object.assign(overlay.style,{transform:`translate(${left}px,${top}px)`,width:Math.max(0,Math.min(right,r.left+box.right)-left)+'px',height:Math.max(0,Math.min(bottom,r.top+box.bottom)-top)+'px'});
   const current=latest.current,hits=boxHits(box,current.geometry,content.clientWidth,current.items.length).map(index=>current.items[index].id),key=hits.join('\n');
   if(key!==shown){shown=key;const added=new Set(hits);current.gestures?.select([...base.filter(item=>!added.has(item)),...hits],hits[0]);}
  };
  const tick=()=>{
   frame=0;if(!active)return;
   // The dock floats over the viewport's bottom padding and can appear mid-gesture, so scrolling starts above it.
   const lower=bottom-parseFloat(getComputedStyle(viewport).paddingBottom||'0');
   const speed=moving?0:pointer.y<view.top+edge?pointer.y-view.top-edge:pointer.y>lower-edge?pointer.y-lower+edge:0,scrolled=viewport.scrollTop;
   if(speed)viewport.scrollTop+=Math.sign(speed)*Math.min(24,Math.ceil(Math.abs(speed)/3));
   paint();
   if(viewport.scrollTop!==scrolled)frame=requestAnimationFrame(tick);
  };
  const begin=()=>{
   active=true;getSelection()?.removeAllRanges();root.classList.add(moving?'dragging-products':'box-selecting');
   try{viewport.setPointerCapture(pointerId);}catch{/* the press already ended */}
   overlay=document.createElement('div');overlay.className=moving?'drag-ghost':'selection-box';
   if(moving)overlay.textContent=ids.length.toLocaleString(viewport.closest<HTMLElement>('[lang]')?.lang||undefined);
   app.append(overlay);
  };
  const move=(event:globalThis.PointerEvent)=>{if(event.pointerId!==pointerId)return;pointer={x:event.clientX,y:event.clientY};if(!active&&Math.hypot(pointer.x-down.x,pointer.y-down.y)>threshold)begin();if(active&&!frame)frame=requestAnimationFrame(tick);};
  const finish=(drop:boolean)=>{
   const category=drop&&moving?target?.dataset.dropCategory:undefined,ended=active;
   active=false;cancelAnimationFrame(frame);mark(null);overlay?.remove();overlay=null;root.classList.remove('dragging-products','box-selecting');
   window.removeEventListener('pointermove',move);window.removeEventListener('pointerup',up);window.removeEventListener('pointercancel',cancel);window.removeEventListener('keydown',escape,true);window.removeEventListener('blur',cancel);window.removeEventListener('selectstart',block);
   try{if(viewport.hasPointerCapture(pointerId))viewport.releasePointerCapture(pointerId);}catch{/* already released */}
   stop.current=null;
   if(!ended)return;
   // The press and release still make a click; it must not reselect the card under the pointer.
   const swallow=(event:MouseEvent)=>{event.stopPropagation();event.preventDefault();};
   window.addEventListener('click',swallow,true);setTimeout(()=>window.removeEventListener('click',swallow,true));
   if(category)latest.current.gestures?.drop(ids,category);
   else if(!drop&&!moving)latest.current.gestures?.select(before);
  };
  const up=(event:globalThis.PointerEvent)=>{if(event.pointerId!==pointerId)return;pointer={x:event.clientX,y:event.clientY};if(active)paint();finish(true);};
  const cancel=()=>finish(false);
  const escape=(event:KeyboardEvent)=>{if(event.key!=='Escape'||!active)return;event.preventDefault();event.stopPropagation();finish(false);};
  const block=(event:Event)=>event.preventDefault();
  window.addEventListener('pointermove',move);window.addEventListener('pointerup',up);window.addEventListener('pointercancel',cancel);window.addEventListener('keydown',escape,true);window.addEventListener('blur',cancel);window.addEventListener('selectstart',block);
  stop.current=cancel;
 };
}
