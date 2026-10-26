// Inert owned HTTPS fixture. Private generated certificate input uses stdin only.
const https=require('node:https'),http=require('node:http'),readline=require('node:readline'),zlib=require('node:zlib');
const input=readline.createInterface({input:process.stdin});
input.once('line',async line=>{
 input.close();process.stdin.pause();process.stdin.unref?.();
 const p=JSON.parse(line),calls=[];let release;const held=new Promise(r=>{release=r;});
 const server=https.createServer({pfx:Buffer.from(p.pfx,'base64'),passphrase:p.password},async(req,res)=>{
  calls.push({method:req.method,path:req.url,authorization:!!req.headers.authorization,cookie:!!req.headers.cookie,referer:!!req.headers.referer});
  if(req.method!=='GET'){res.writeHead(405);res.end();return;}
  const type='text/html; charset=utf-8';
  if(req.url==='/retry'){res.writeHead(429,{'retry-after':'2'});res.end();return;}
  if(req.url==='/unsafe'){res.writeHead(302,{location:'http://127.0.0.1:1/forbidden'});res.end();return;}
  if(req.url==='/redirect'||req.url==='/loop'){res.writeHead(302,{location:req.url==='/redirect'?'/page':'/loop'});res.end();return;}
  if(req.url==='/slow'){res.writeHead(200,{'content-type':type});res.write('<p>held');await held;res.end('</p>');return;}
  if(req.url==='/oversize'||req.url==='/compressed'){const raw=Buffer.alloc(256*1024+1,120);res.writeHead(200,{'content-type':'text/plain',...(req.url==='/compressed'?{'content-encoding':'gzip'}:{})});res.write(req.url==='/compressed'?zlib.gzipSync(raw):raw);await held;res.end();return;}
  const body=req.url==='/captcha'?'<p>verify you are human</p>':req.url==='/json'?'{}':req.url==='/encoding'?'legacy':req.url==='/invalid-utf8'?Buffer.from([0xff]):req.url==='/exact'?'x'.repeat(256*1024):'<html><script>secret-script < malicious</script><style>secret-style</style><form>secret-form</form><p hidden>secret-hidden</p><p>Owned &amp; visible 正文 😀</p><p>Second paragraph</p></html>';
  res.writeHead(200,{'content-type':req.url==='/json'?'application/json':req.url==='/encoding'?'text/html; charset=gbk':req.url==='/exact'?'text/plain':type});res.end(body);
 });
 const control=http.createServer((req,res)=>{if(req.url==='/release')release();res.setHeader('content-type','application/json');res.end(JSON.stringify({calls}));});
 for(const s of [server,control])await new Promise((resolve,reject)=>{s.once('error',reject);s.listen(0,'127.0.0.1',resolve);});
 process.stdout.write(JSON.stringify({port:server.address().port,control:control.address().port,root_der:p.root_der})+'\n');
});
