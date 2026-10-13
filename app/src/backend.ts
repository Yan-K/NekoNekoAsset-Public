import {invoke, convertFileSrc, isTauri} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';
import {getCurrentWebview} from '@tauri-apps/api/webview';
import type {Product, Account, Locale} from './model';

export type Service = 'catalogue'|'booth'|'storage'|'unity';
export interface SourceProgress {stage?:string;source?:string;pages?:number;detailsCompleted?:number;requestsCompleted?:number;products?:number;published?:number;metadataCompleted?:number;metadataTotal?:number;availabilityWarnings?:number}
export interface NativeJob extends UnityPreparation {id:string;type:string;productId?:string;productIds?:string[];completedProductIds?:string[];fileIds?:string[];selectedFileIds?:string[]|null;accountId?:string;accountName?:string;state:string;progress?:number|SourceProgress;errorCode?:string;createdAt?:string;updatedAt?:string;completedAt?:string;projectName?:string;unityVersion?:string;stage?:string;writesStarted?:boolean;resultCounts?:{applied:number;skipped:number;failed:number};compileErrorCount?:number}
export interface UnityPreparation {preparationPhase?:string|null;preparationWaitingForEditor?:boolean;preparationBytes?:number|null;preparationTotalBytes?:number|null;preparationEntries?:number|null;preparationTotalEntries?:number|null}
export function progressPercent(job:(NativeJob&UnityPreparation)|undefined):number|undefined{
 if(job?.type==='unity:import'&&['waitingForUser','importing','refreshing','compiling','completed'].includes(job.stage??''))return 100;
 if(typeof job?.progress==='number')return job.progress;
 if(job?.stage!=='preparing')return undefined;
 if(!job.preparationPhase||job.preparationWaitingForEditor)return undefined;
 const [done,total]=job.preparationPhase==='checkTargets'?[job.preparationEntries,job.preparationTotalEntries]:[job.preparationBytes,job.preparationTotalBytes];
 return typeof done==='number'&&typeof total==='number'&&total>0?Math.min(100,100*done/total):undefined;
}
export interface NativeRoot {id:string;path:string;label?:string;status:string;usedBytes?:number;freeBytes?:number}
export interface AvatarOption {id:string;productId?:string;displayName?:string;officialName:string;englishName?:string;labelStatus?:string;shopName?:string;shopId?:string;aliases:string[];matchingProductCount?:number;baseProductIds?:string[];enabled?:boolean;explicitEnabled?:boolean}
export interface AvatarSetting {enabled:boolean;originalName?:string;englishName?:string;aliases?:string[]}
export interface TagOption {id:string;label:string;kind:string;aliases:string[]}
export interface FetchedProductMetadata {sourceUrl:string;title:string;category:Product['category'];tags:string[];thumbnailUrl:string|null}
export interface LocalFileChanges {draftId:string;addSources:string[];removeFileIds:string[]}
export async function updateLocalFiles(productId:string,changes:LocalFileChanges):Promise<{jobId:string;productIds:string[];state:string}>{return invokeAction('storage','local_files',{productId,...changes});}
export interface NativeCollection {id:string;name:string;productIds:string[]}
export interface LibraryCredential {id:string;name:string;store:string}
export interface LibraryStatus {state:'ready'|'setup'|'unconfigured'|'blocked';path?:string;defaultPath?:string;errorCode?:string;locale?:string;missingCredentials?:LibraryCredential[]}
export async function libraryStatus():Promise<LibraryStatus>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('library_status');}
export async function openLibrary(path:string,locale?:Locale,newLibrary=false):Promise<{restarting:boolean}>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('library_open',{path,locale,newLibrary});}
export interface NativeState {version:number;products:Product[];accounts:Account[];roots:NativeRoot[];jobs:NativeJob[];preferences:Record<string,unknown>;collections:Record<string,unknown>[];savedSearches:Record<string,unknown>[];sourceCoverage:Record<string,unknown>[];conflicts:Record<string,unknown>[];avatarOptions?:AvatarOption[];ownedBaseOptions?:AvatarOption[];tagOptions?:TagOption[];total?:number;counts?:{all:number;downloaded:number;attention:number;hidden?:number}}
export const emptyState:NativeState={version:1,products:[],accounts:[],roots:[],jobs:[],preferences:{},collections:[],savedSearches:[],sourceCoverage:[],conflicts:[]};
export interface DialogResult {path:string;content?:string;paths?:string[]}
export async function invokeAction<T=any>(service:Service,action:string,args:Record<string,unknown>={}):Promise<T>{
 if(!isTauri())throw new Error('nativeUnavailable');
 return invoke<T>(`${service}_action`,{action,args});
}
export async function dialog(kind:'folder'|'open_file'|'save_file',options:Record<string,unknown>={}):Promise<DialogResult|null>{
 if(!isTauri())throw new Error('nativeUnavailable');
 const result=await invoke<DialogResult|string|string[]|null>('native_dialog',{kind,options});
 return typeof result==='string'?{path:result}:Array.isArray(result)?{path:result[0]??'',paths:result}:result;
}
export interface NativeChange {scope?:string;productIds?:string[]}
export async function subscribeChanged(reload:(change:NativeChange)=>void):Promise<()=>void>{return isTauri()?listen<{scope?:unknown;productIds?:unknown}>('nna:changed',event=>reload({scope:typeof event.payload?.scope==='string'?event.payload.scope:undefined,productIds:Array.isArray(event.payload?.productIds)?event.payload.productIds.filter((id):id is string=>typeof id==='string'):undefined})):()=>{};}
export async function nativeFileAction<T=any>(action:string,args:Record<string,unknown>):Promise<T>{if(!isTauri())throw new Error('nativeUnavailable');return invoke<T>('native_file_action',{action,args});}
export async function nativeZoom(scale:number):Promise<{scale:number}>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('native_zoom',{scale});}
export interface AiKeyStatus {entryId?:string|null;configured?:boolean;provider:string;endpoint:string;model:string;keyPresent:boolean;visionSupported:boolean;keyRequired:boolean}
export interface AiProviderPreset {id:string;label:string;endpoint:string;model:string;wire:string;visionSupported:boolean;keyRequired:boolean}
export interface AiConnectionResult {ok:true;provider:string;model:string}
export interface AiEntry {id:string;provider:string;endpoint:string;model:string;keyPresent:boolean;visionSupported:boolean;keyRequired:boolean}
export interface AiEntryList {entries:AiEntry[];activeEntryId:string|null}
export interface AiDraft {provider:string;endpoint:string;model:string;key:string}
export interface AiDraftTest {ok:true;testToken:string}
export type AiRequest = {action:'key_status'|'test_connection'|'providers'|'list_entries'}|({action:'test_draft'}&AiDraft)|({action:'save_entry';testToken:string}&AiDraft)|{action:'select_entry'|'remove_entry'|'test_entry';entryId:string};
async function invokeAi<T>(request:AiRequest):Promise<T>{if(!isTauri())throw new Error('nativeUnavailable');return invoke<T>('ai_action',{request});}
export function getAiKeyStatus():Promise<AiKeyStatus>{return invokeAi({action:'key_status'});}
export function testAiConnection():Promise<AiConnectionResult>{return invokeAi({action:'test_connection'});}
export function getAiProviders():Promise<{presets:AiProviderPreset[]}>{return invokeAi({action:'providers'});}
export function listAiEntries():Promise<AiEntryList>{return invokeAi({action:'list_entries'});}
export function testAiDraft(draft:AiDraft):Promise<AiDraftTest>{return invokeAi({action:'test_draft',...draft});}
export function saveAiEntry(draft:AiDraft&{testToken:string}):Promise<AiEntryList>{return invokeAi({action:'save_entry',...draft});}
export function selectAiEntry(entryId:string):Promise<AiEntryList>{return invokeAi({action:'select_entry',entryId});}
export function removeAiEntry(entryId:string):Promise<AiEntryList>{return invokeAi({action:'remove_entry',entryId});}
export function testAiEntry(entryId:string):Promise<AiConnectionResult>{return invokeAi({action:'test_entry',entryId});}
export interface AiImage {mimeType:'image/jpeg'|'image/png';dataBase64:string}
export interface AiChatMessage {role:'user'|'assistant';content:string}
export interface AiSuggestionResponse {chatId?:string;answer:string;recommendations:{candidateId:string;origin:'library'|'store';productId?:string;title:string;provider:string;url?:string;thumbnailUrl?:string;thumbnailPath?:string;reason:string}[];searchStatus:{provider:string;status:'ok'|'unavailable'|'no_results'}[]}
export interface AiSuggestionInput {requestId:string;chatId:string;entryId?:string;appLanguage:import('./model').Locale;prompt:string;preset:'outfit'|'similar'|'accessories'|'chat';scope:'library'|'online'|'both';stores:('booth'|'gumroad'|'jinxxy')[];category?:string;avatarId?:string;contextProductIds?:string[];history?:AiChatMessage[];images:AiImage[]}
export async function sendAiSuggestion(input:AiSuggestionInput):Promise<AiSuggestionResponse>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('ai_suggestion_action',{request:{action:'send',...input}});}
export async function cancelAiSuggestion(requestId:string):Promise<unknown>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('ai_suggestion_action',{request:{action:'cancel',requestId}});}
export interface AiScopes {title:boolean;tags:boolean;description:boolean;files:boolean;images:boolean}
export type AiMode='classify'|'organize';
type AiAnalysisRequest={action:'start';mode:AiMode;productIds:string[];scopes:AiScopes}|{action:'cancel'|'retry';jobId:string}|{action:'dismiss_source_url';productId:string;url:string};
async function invokeAiAnalysis<T>(request:AiAnalysisRequest):Promise<T>{if(!isTauri())throw new Error('nativeUnavailable');return invoke<T>('ai_analysis_action',{request});}
export function startAi(request:{mode:AiMode;productIds:string[];scopes:AiScopes}):Promise<{jobId:string}>{return invokeAiAnalysis({action:'start',...request});}
export function cancelAi(jobId:string):Promise<unknown>{return invokeAiAnalysis({action:'cancel',jobId});}
export function retryAi(jobId:string):Promise<{jobId:string}>{return invokeAiAnalysis({action:'retry',jobId});}
export function dismissAiSourceUrlCandidate(request:{productId:string;url:string}):Promise<unknown>{return invokeAiAnalysis({action:'dismiss_source_url',...request});}
export function saveAiPreferences(patch:{aiEnabled:boolean;aiScopes:AiScopes}):Promise<unknown>{return invokeAction('catalogue','preferences',{patch});}
export async function openExternal(url:string):Promise<void>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('external_open',{url});}
export interface AppRelease {version:string;notes:string;releaseUrl:string}
export interface BridgeRelease {version:string;unitypackageUrl:string;releaseUrl:string}
export interface UpdateCheck {app:{currentVersion:string;installSupported:boolean;available?:AppRelease;errorCode?:string};bridge?:BridgeRelease;bridgeError?:string}
export interface UpdateProgress {stage:'downloading'|'verifying'|'installing';downloadedBytes:number;totalBytes?:number|null}
export async function checkUpdates():Promise<UpdateCheck>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('updates_check');}
export async function installUpdate(version:string):Promise<void>{if(!isTauri())throw new Error('nativeUnavailable');return invoke('updates_install',{version});}
export async function subscribeUpdateProgress(callback:(progress:UpdateProgress)=>void):Promise<()=>void>{return isTauri()?listen<UpdateProgress>('updates:progress',event=>callback(event.payload)):()=>{};}
export async function fetchProductMetadata(url:string):Promise<FetchedProductMetadata>{return invokeAction('booth','fetch_product_metadata',{url});}
export function previewSource(path:string|undefined):string|undefined{if(!path)return undefined;if(/^(https:\/\/|asset:|https?:\/\/asset\.localhost\/)/.test(path))return path;return isTauri()?convertFileSrc(path):undefined;}
/** Only stable codes reach UI; private response text and URLs are never surfaced. */
export function errorCode(error:unknown):string {const value=typeof error==='string'?error:error instanceof Error?error.message:error&&typeof error==='object'&&'code' in error&&typeof error.code==='string'?error.code:'';return /^[A-Za-z][A-Za-z0-9_.-]{0,80}$/.test(value)?value:'operationFailed';}

export interface UnityEditor {instanceId:string;sessionId:string;projectName:string;projectRoot:string;unityVersion:string;bridgeVersion?:string|null;busy:boolean}
export interface UnityLeaf {leafRef:string;productId:string;productTitle:string;fileId:string;sourceName:string;variantName:string;revision:string;memberPath:string;displayName:string;relativePath:string;kind:'unitypackage'|'file';bytes:number;sha256:string}
export interface UnityReceipt extends UnityPreparation {requestId:string;fingerprint:string;instanceId:string;sessionId:string;stage:string;staged:boolean;writesStarted:boolean;updatedUtc:string;error?:string|null;compileErrors:string[];results:{entryId:string;path:string;state:'applied'|'skipped'|'failed';message?:string|null}[]}
export interface UnityDetails {jobId:string;projectName:string;projectRoot:string;unityVersion:string;stage:string;errorCode?:string|null;receipt:UnityReceipt|null}
export const listUnityEditors=()=>invokeAction<{editors:UnityEditor[]}>('unity','list_editors');
export const unityInventory=(productIds:string[])=>invokeAction<{files:UnityLeaf[]}>('unity','inventory',{productIds});
export const submitUnity=(input:{productIds:string[];leafRefs:string[];instanceId:string;sessionId:string;locale?:string})=>invokeAction<{jobId:string}>('unity','submit',input);
export const unityDetails=(jobId:string)=>invokeAction<UnityDetails>('unity','details',{jobId});
export const queryUnity=(jobId:string)=>invokeAction<UnityDetails>('unity','status',{jobId});
export const cancelUnity=(jobId:string)=>invokeAction<UnityDetails>('unity','cancel',{jobId});
export const retryUnityInventory=(jobId:string)=>invokeAction<{productIds:string[];leafRefs:string[]}>('unity','retry_inventory',{jobId});

export type {AiPersonalization,AiPersonalityId} from './ai-personalization';
export interface AiChatSummary {id:string;title:string;presetId:string;createdAt:string;updatedAt:string}
export interface AiStoredTurn {id:string;role:'user'|'assistant';content:string;images:AiImage[];createdAt:string;status?:'pending'|'sent'|'failed'|'cancelled';response?:AiSuggestionResponse}
export interface AiChatDetail {chat:AiChatSummary;messages:AiStoredTurn[]}
export interface AiCustomPreset {id:string;title:string;description:string;systemPrompt:string}
export interface AiPresetDraft {id?:string;title:string;description:string;systemPrompt:string}
export interface AiWorkspace {chats:AiChatSummary[];presets:AiCustomPreset[];personalization:import('./ai-personalization').AiPersonalization}
async function invokeAiWorkspace<T>(action:string,args:Record<string,unknown>={}):Promise<T>{if(!isTauri())throw new Error('nativeUnavailable');return invoke<T>('ai_suggestion_action',{request:{action,...args}});}
export function listAiWorkspace():Promise<AiWorkspace>{return invokeAiWorkspace('list_workspace');}
export function createAiChat(presetId:string):Promise<AiChatDetail>{return invokeAiWorkspace('create_chat',{presetId});}
export function getAiChat(chatId:string):Promise<AiChatDetail>{return invokeAiWorkspace('get_chat',{chatId});}
export async function renameAiChat(chatId:string,title:string):Promise<AiChatSummary>{const result=await invokeAiWorkspace<{chat:AiChatSummary}>('rename_chat',{chatId,title});return result.chat;}
export function deleteAiChat(chatId:string):Promise<unknown>{return invokeAiWorkspace('delete_chat',{chatId});}
export async function saveAiPersonalization(personalization:import('./ai-personalization').AiPersonalization):Promise<import('./ai-personalization').AiPersonalization>{const result=await invokeAiWorkspace<{personalization:import('./ai-personalization').AiPersonalization}>('save_personalization',{personalization});return result.personalization;}
export async function saveAiPreset(preset:AiPresetDraft):Promise<AiCustomPreset>{const result=await invokeAiWorkspace<{preset:AiCustomPreset}>('save_preset',{preset});return result.preset;}
export function deleteAiPreset(presetId:string):Promise<unknown>{return invokeAiWorkspace('delete_preset',{presetId});}
export async function importAiPreset(json:string):Promise<AiCustomPreset>{const result=await invokeAiWorkspace<{preset:AiCustomPreset}>('import_preset',{json});return result.preset;}
export function exportAiPreset(presetId:string):Promise<{json:string}>{return invokeAiWorkspace('export_preset',{presetId});}
export async function readAiDroppedImages(paths:string[]):Promise<AiImage[]>{const result=await invokeAiWorkspace<{images:AiImage[]}>('read_images',{paths});return result.images;}
export async function subscribeAiImageDrop(callback:(paths:string[])=>void):Promise<()=>void>{if(!isTauri())return()=>{};return getCurrentWebview().onDragDropEvent(event=>{if(event.payload.type==='drop')callback(event.payload.paths);});}
