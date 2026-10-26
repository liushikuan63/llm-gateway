$ErrorActionPreference = 'Stop'
# Owned fixture only: keys remain in memory; JSON stdout is a private pipe to Node.
$source = @'
using System;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
public static class LlmGatewayOwnedCertificate {
 public static string Generate() {
  using var caKey=RSA.Create(2048);using var leafKey=RSA.Create(2048);
  X509Certificate2 leaf;
  var now=DateTimeOffset.UtcNow;
  var root=new CertificateRequest("CN=LLMGW owned N4 temporary test root",caKey,HashAlgorithmName.SHA256,RSASignaturePadding.Pkcs1);
  root.CertificateExtensions.Add(new X509BasicConstraintsExtension(true,false,0,true));
  root.CertificateExtensions.Add(new X509KeyUsageExtension(X509KeyUsageFlags.KeyCertSign|X509KeyUsageFlags.CrlSign,true));
  using var ca=root.CreateSelfSigned(now.AddMinutes(-2),now.AddDays(2));
  var request=new CertificateRequest("CN=owned-upstream.invalid",leafKey,HashAlgorithmName.SHA256,RSASignaturePadding.Pkcs1);
  request.CertificateExtensions.Add(new X509BasicConstraintsExtension(false,false,0,true));
  request.CertificateExtensions.Add(new X509KeyUsageExtension(X509KeyUsageFlags.DigitalSignature|X509KeyUsageFlags.KeyEncipherment,true));
  var names=new SubjectAlternativeNameBuilder(); names.AddDnsName("owned-upstream.invalid");request.CertificateExtensions.Add(names.Build());
  var usage=new OidCollection();usage.Add(new Oid("1.3.6.1.5.5.7.3.1"));request.CertificateExtensions.Add(new X509EnhancedKeyUsageExtension(usage,true));
  var serial=new byte[16];RandomNumberGenerator.Fill(serial);
  using(var signed=request.Create(ca,now.AddMinutes(-1),now.AddDays(1),serial)) {leaf=signed.CopyWithPrivateKey(leafKey);}

  using(leaf) {
   var bytes=new byte[24];RandomNumberGenerator.Fill(bytes);string password=Convert.ToBase64String(bytes);
   string pfx=Convert.ToBase64String(leaf.Export(X509ContentType.Pfx,password));
   string der=Convert.ToBase64String(ca.Export(X509ContentType.Cert));
   return "{\"root_der\":\""+der+"\",\"pfx\":\""+pfx+"\",\"password\":\""+password+"\"}";
  }
 }
}
'@
Add-Type -TypeDefinition $source -Language CSharp
[Console]::WriteLine([LlmGatewayOwnedCertificate]::Generate())
