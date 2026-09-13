'use strict';
// Fixed protocol adapter, not a general JavaScript runner. All HTTP is fulfilled by the Rust
// policy broker. A dead-end proxy also blocks browser traffic that escapes route interception.
const readline = require('node:readline');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.argv[1]);
let privateDirectory;
let browser, context, page, active = false, generation = 0, networkId = 0;
let network = new Map();
let running = false, preview = false, stopping = false, currentAction, shutdownTask;
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const bounded = (text, limit) => String(text || '').slice(0, limit);
async function snapshot() {
  const gen = ++generation;
  const nodes = await page.locator('a,button,input,textarea,select,[role],[tabindex]').evaluateAll((elements, gen) => {
    return elements.slice(0,200).map((element,index) => {
      const ref = `e${gen}-${index}`;
      element.setAttribute('data-heycode-ref',ref);
      return {ref,tag:element.tagName.toLowerCase(),role:element.getAttribute('role'),
        label:(element.getAttribute('aria-label') || element.getAttribute('placeholder') || '').slice(0,256),
        text:(element.innerText || '').slice(0,256), type:element.getAttribute('type'),
        value:element.type==='password' ? '[redacted]' : String(element.value || '').slice(0,256)};
    });
  },gen);
  return {url:bounded(page.url(),8192),title:bounded(await page.title(),512),
    text:bounded(await page.locator('body').innerText({timeout:3000}),32768),
    accessibility:bounded(await page.locator('body').ariaSnapshot({timeout:3000}),32768),
    elements:nodes,limits:{text:32768,accessibility:32768,elements:200}};
}
async function launch() {
  privateDirectory = fs.mkdtempSync(path.join(process.cwd(), '.heycode-browser-'));
  fs.chmodSync(privateDirectory, 0o700);
  // Chromium on macOS uses MAC_CHROMIUM_TMPDIR rather than TMPDIR for singleton
  // sockets. Both are private to this fixed adapter, underneath the authorized cwd.
  process.env.TMPDIR = privateDirectory;
  process.env.MAC_CHROMIUM_TMPDIR = privateDirectory;
  browser = await chromium.launch({executablePath:process.argv[2],headless:true,chromiumSandbox:true,
    downloadsPath:privateDirectory,
    proxy:{server:'http://127.0.0.1:9',bypass:'<-loopback>'},
    args:['--disable-quic','--force-webrtc-ip-handling-policy=disable_non_proxied_udp','--disable-features=WebTransport',
      '--disable-crash-reporter','--disable-breakpad',`--crash-dumps-dir=${privateDirectory}`,`--disk-cache-dir=${path.join(privateDirectory,'cache')}`]});
  context = await browser.newContext({viewport:{width:1280,height:800},acceptDownloads:false,
    serviceWorkers:'block',permissions:[]});
  context.setDefaultTimeout(10000);
  await context.routeWebSocket('**/*', websocket => websocket.close());
  await context.route('**/*',async route => {
    if (stopping || !active || preview || network.size>=32) { await route.abort().catch(()=>{}); return; }
    const request = route.request();
    if (!/^https?:/.test(request.url())) { await route.abort().catch(()=>{}); return; }
    // Redirects are never followed by the broker; each next hop is a fresh admission.
    let hops=0; for(let r=request.redirectedFrom();r;r=r.redirectedFrom()) if(++hops>5){await route.abort();return;}
    const body=request.postDataBuffer();
    if (body && body.length>1048576) { await route.abort(); return; }
    const id=++networkId;
    const reply = new Promise(resolve=>network.set(id,resolve));
    send({kind:'http',id,main_navigation:request.isNavigationRequest() && request.frame()===page.mainFrame(),url:request.url(),method:request.method(),headers:await request.allHeaders(),body:body?body.toString('base64'):''});
    const response=await reply;
    try {
      if(response.error) await route.abort();
      else await route.fulfill({status:response.status,headers:response.headers,body:Buffer.from(response.body,'base64')});
    } catch {} finally { network.delete(id); }
  });
  page=await context.newPage();
  page.on('dialog',dialog=>dialog.dismiss().catch(()=>{}));
  context.on('page',extra=>{if(extra!==page) extra.close().catch(()=>{});});
}
async function action(message) {
  if(message.action==='launch'){ await launch(); return {ready:true}; }
  if(!page) throw new Error('not_open');
  active=true;
  try {
    switch(message.action) {
      case 'navigate':
        preview=false;
        if(!/^https?:/.test(message.url)) throw new Error('scheme');
        await page.goto(message.url,{waitUntil:'domcontentloaded',timeout:20000}); break;
      case 'inspect': break;
      case 'click': case 'type': {
        if(!new RegExp(`^e${generation}-[0-9]+$`).test(message.element)) throw new Error('stale_element');
        const locator=page.locator(`[data-heycode-ref="${message.element}"]`);
        if(await locator.count()!==1) throw new Error('stale_element');
        if(message.action==='click') await locator.click();
        else await locator.fill(message.text);
        break;
      }
      case 'screenshot': {
        const bytes=await page.screenshot({type:'png',fullPage:false,timeout:10000});
        if(bytes.length>8388608) throw new Error('screenshot_limit');
        return {png:bytes.toString('base64'),width:1280,height:800};
      }
      case 'preview':
        preview=true;
        // Local generated HTML is rendered with its own opaque origin and no external network.
        active=false;
        await page.goto('about:blank');
        await page.setContent(message.html,{waitUntil:'domcontentloaded',timeout:10000}); break;
      default: throw new Error('unknown_action');
    }
    return await snapshot();
  } finally {
    active=false;
    // Unsettled requests are denied; no work inherits authority from a finished tool call.
    for(const resolve of network.values()) resolve({error:true});
  }
}
const input=readline.createInterface({input:process.stdin,crlfDelay:Infinity});
function shutdown() {
  if(shutdownTask) return shutdownTask;
  stopping=true;
  active=false;
  for(const resolve of network.values())resolve({error:true});
  shutdownTask=(async()=>{
    // A launch may still own a Chromium process before assigning browser. Wait for
    // that action before cleanup; a hard-killed adapter safely leaves its directory.
    await currentAction?.catch(()=>{});
    await browser?.close();
    if(privateDirectory) fs.rmSync(privateDirectory,{recursive:true,force:true});
  })();
  return shutdownTask;
}
input.on('line',line=>{
  if(line.length>12*1024*1024){process.exitCode=1;input.close();return;}
  let message; try{message=JSON.parse(line);}catch{process.exitCode=1;input.close();return;}
  if(message.kind==='http_result'){network.get(message.id)?.(message);return;}
  if(message.kind==='shutdown'){shutdown().then(()=>{send({kind:'shutdown'});process.exit(0);},()=>process.exit(1));return;}
  if(running || stopping){send({kind:'result',error:'protocol_busy'});return;}
  running=true;
  currentAction=action(message).then(value=>send({kind:'result',value}),error=>send({kind:'result',error:error.message==='stale_element'?'stale_element':error.name==='TimeoutError'?'timeout':/sandbox initialization failed|Failed to initialize sandbox/.test(String(error))?'browser_sandbox_failed':'browser_action_failed'})).finally(()=>{running=false;});
});
input.on('close',()=>{shutdown().then(()=>process.exit(0),()=>process.exit(1));});
