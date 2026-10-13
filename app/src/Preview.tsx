import {useEffect,useRef,useState} from 'react';
import {convertFileSrc,invoke,isTauri} from '@tauri-apps/api/core';
import Art from './Art';
import {previewSource} from './backend';

type PreviewRequest={promise:Promise<string|undefined>;path?:string};
const requests=new Map<string,PreviewRequest>();
function cachedPreview(url:string):Promise<string|undefined>{
 const existing=requests.get(url);if(existing)return existing.promise;
 const request:PreviewRequest={promise:Promise.resolve(undefined)};
 request.promise=isTauri()?invoke<{path:string}>('preview_cache',{url}).then(result=>{request.path=convertFileSrc(result.path);return request.path;}).catch(()=>{if(requests.get(url)===request)requests.delete(url);return undefined;}):request.promise;
 requests.set(url,request);if(requests.size>256)requests.delete(requests.keys().next().value!);
 return request.promise;
}
function forgetPreview(url:string,path:string){
 if(requests.get(url)?.path===path)requests.delete(url);
}

/** Public images load through the bounded native cache; list overscan stays idle. */
export default function Preview({src,title,placeholderTitle=title,className,lazy=false}:{src?:string;title:string;placeholderTitle?:string;className?:string;lazy?:boolean}){
 const node=useRef<Element|null>(null);
 const [visibleSource,setVisibleSource]=useState<string>();
 const [cached,setCached]=useState<{source:string;path?:string}>();
 const [failed,setFailed]=useState<string>();
 const [retry,setRetry]=useState(0);
 const retriedSource=useRef<string|undefined>(undefined);
 const remote=!!src&&/^https:\/\//i.test(src)&&!/^https:\/\/asset\.localhost\//i.test(src);
 const enabled=!lazy||visibleSource===src;
 useEffect(()=>{retriedSource.current=undefined;setFailed(undefined);},[src]);
 useEffect(()=>{
  if(!lazy||!remote||!src||!node.current)return;
  const observer=new IntersectionObserver(entries=>{if(entries.some(entry=>entry.isIntersecting)){setVisibleSource(src);observer.disconnect();}});
  observer.observe(node.current);return()=>observer.disconnect();
 },[src,lazy,remote]);
 useEffect(()=>{
  if(!remote||!src||!enabled)return;
  let current=true;void cachedPreview(src).then(path=>{if(current)setCached({source:src,path});});
  return()=>{current=false;};
 },[src,remote,enabled,retry]);
 const image=remote?(cached?.source===src?cached.path:undefined):previewSource(src);
 const imageFailed=()=>{
  if(!image)return;
  setFailed(image);
  if(remote&&src&&retriedSource.current!==src){
   retriedSource.current=src;forgetPreview(src,image);setCached(undefined);setFailed(undefined);setRetry(value=>value+1);
  }
 };
 return image&&failed!==image?<img draggable={false} ref={element=>{node.current=element;}} className={className} src={image} loading={lazy?'lazy':undefined} alt={title} onError={imageFailed}/>:<Art title={placeholderTitle} elementRef={element=>{node.current=element;}}/>;
}
