#!/usr/bin/env bash
# 为 mailcraft-ohos 生成一份【本地】调试签名（OpenHarmony 本地 CA 链），
# 绑独立 bundle 名 cn.shaliuxin.mailcraft（与用户其它 craft 工程不冲突）。
# 产出 signing/ 下的 .cer/.p12/.p7b，供 build-profile.json5 引用。
#
# 注意：这是本地自签 CA 链。HarmonyOS 7.0 真机可能拒绝纯本地签名；若安装被拒，
# 请在 DevEco Studio 里把 bundle 名改为本名、用"自动签名"让 AGC 签发正式调试
# profile（DevEco 会自动回写 material 四件套到 build-profile.json5）。
#
# 包名铁律：bundleName 绑定 profile 与设备 UDID，上架后永久不可改；
# 切勿借用其它 craft 工程的 profile（会与该包名旧版撞车，报 9568263 版本降级）。
set -euo pipefail

DEVECO="${DEVECO_HOME:-/c/Program Files/Huawei/DevEco Studio}"
JAVA="$DEVECO/jbr/bin/java.exe"
SIGTOOL="$DEVECO/sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/signing"
BUNDLE="${MC_BUNDLE:-cn.shaliuxin.mailcraft}"
KEYALIAS="mailcraft-debug"
# 口令与设备 UDID 不入库：从环境变量读取（本地调试时自行 export）。
#   export MC_KEYPW=... MC_STOREPW=... MC_UID1=<设备UDID> MC_UID2=<设备UDID>
# 切勿把真实口令/UDID 写回脚本或提交到公开仓库。
KEYPW="${MC_KEYPW:?请 export MC_KEYPW（本地调试 CA 密钥口令，勿入库）}"
STOREPW="${MC_STOREPW:?请 export MC_STOREPW（本地调试 CA 库口令，勿入库）}"
UID1="${MC_UID1:?请 export MC_UID1（设备 UDID，勿入库）}"
UID2="${MC_UID2:?请 export MC_UID2（设备 UDID，勿入库）}"

mkdir -p "$OUT"
cd "$OUT"
echo "bundle=$BUNDLE out=$OUT"
run() { echo "+ $JAVA -jar $SIGTOOL $*"; "$JAVA" -jar "$SIGTOOL" "$@" 2>&1 | grep -iE "success|error|exception" | tail -2 || true; }

# 1) 根 CA 密钥对
rm -f root.jks root-ca.cer app-sub.jks app-sub-ca.cer app.p12 app-cert.cer profile.jks profile-ca.cer profile-cert.cer app-profile.json mailcraft-debug.p7b
run generate-keypair -keyAlias oh-root-ca -keyAlg ECC -keySize NIST-P-256 \
  -keystoreFile root.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW"

# 2) 自签根证书
run generate-ca -keyAlias oh-root-ca -keyAlg ECC -keySize NIST-P-256 -signAlg SHA256withECDSA \
  -subject "C=CN,O=mailcraft,OU=dev,CN=MailCraft Root CA" \
  -keystoreFile root.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW" \
  -outFile root-ca.cer

# 3) 应用签名子 CA 密钥对（由根 CA 签发）
run generate-keypair -keyAlias oh-app-sub-ca -keyAlg ECC -keySize NIST-P-256 \
  -keystoreFile app-sub.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW"
run generate-ca -keyAlias oh-app-sub-ca -keyAlg ECC -keySize NIST-P-256 -signAlg SHA256withECDSA \
  -issuer "C=CN,O=mailcraft,OU=dev,CN=MailCraft Root CA" \
  -issuerKeyAlias oh-root-ca -issuerKeyPwd "$KEYPW" -issuerKeystorePwd "$STOREPW" -issuerKeystoreFile root.jks \
  -subject "C=CN,O=mailcraft,OU=dev,CN=MailCraft App Signing CA" \
  -keystoreFile app-sub.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW" \
  -outFile app-sub-ca.cer

# 4) 应用密钥对（p12，供 hvigor storeFile）
run generate-keypair -keyAlias "$KEYALIAS" -keyAlg ECC -keySize NIST-P-256 \
  -keystoreFile app.p12 -keystorePwd "$STOREPW" -keyPwd "$KEYPW"

# 5) 应用签名证书（由应用子 CA 签发，含 root+sub 双链）
run generate-app-cert -keyAlias "$KEYALIAS" -keyPwd "$KEYPW" \
  -issuer "C=CN,O=mailcraft,OU=dev,CN=MailCraft App Signing CA" \
  -issuerKeyAlias oh-app-sub-ca -issuerKeyPwd "$KEYPW" -issuerKeystorePwd "$STOREPW" -issuerKeystoreFile app-sub.jks \
  -subject "C=CN,O=mailcraft,OU=dev,CN=mailcraft-ohos" \
  -validity 3650 -signAlg SHA256withECDSA \
  -keystoreFile app.p12 -keystorePwd "$STOREPW" \
  -outForm certChain -rootCaCertFile root-ca.cer -subCaCertFile app-sub-ca.cer \
  -outFile app-cert.cer

# 6) profile 签名子 CA 密钥对
run generate-keypair -keyAlias oh-profile-ca -keyAlg ECC -keySize NIST-P-256 \
  -keystoreFile profile.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW"

# 7) profile 签名 CA 证书（由根 CA 签发）
run generate-ca -keyAlias oh-profile-ca -keyAlg ECC -keySize NIST-P-256 -signAlg SHA256withECDSA \
  -issuer "C=CN,O=mailcraft,OU=dev,CN=MailCraft Root CA" \
  -issuerKeyAlias oh-root-ca -issuerKeyPwd "$KEYPW" -issuerKeystorePwd "$STOREPW" -issuerKeystoreFile root.jks \
  -subject "C=CN,O=mailcraft,OU=dev,CN=MailCraft Profile CA" \
  -keystoreFile profile.jks -keystorePwd "$STOREPW" -keyPwd "$KEYPW" \
  -outFile profile-ca.cer

# 7b) provision profile 证书（由 profile CA 签发，含双 CA 链）
run generate-profile-cert -keyAlias oh-profile-ca -keyPwd "$KEYPW" \
  -issuer "C=CN,O=mailcraft,OU=dev,CN=MailCraft Profile CA" \
  -issuerKeyAlias oh-root-ca -issuerKeyPwd "$KEYPW" -issuerKeystorePwd "$STOREPW" -issuerKeystoreFile root.jks \
  -rootCaCertFile root-ca.cer -subCaCertFile profile-ca.cer \
  -outForm certChain -keystoreFile profile.jks -keystorePwd "$STOREPW" -outFile profile-cert.cer

# 8) 生成 profile json（含 bundle 名 + 设备 UDID）
NB=$(date -u +%s); NA=$((NB + 3*365*24*3600))
APP_B64=$(sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' app-cert.cer | tr -d '\n' | base64 | tr -d '\n')
cat > app-profile.json <<EOF
{
  "version-name": "0.15.0",
  "version-code": 1500,
  "uuid": "6c1a0e2b-4f3d-4b7a-9c2e-mailcraft001",
  "validity": { "not-before": $NB, "not-after": $NA },
  "type": "debug",
  "bundle-info": {
    "developer-id": "55530755",
    "development-certificate": "$APP_B64",
    "bundle-name": "$BUNDLE",
    "apl": "normal",
    "app-feature": "hos_normal_app",
    "app-identifier": "6917620714977119981"
  },
  "baseapp-info": {},
  "permissions": {},
  "debug-info": { "device-ids": ["$UID1", "$UID2"], "device-id-type": "udid" },
  "acls": { "allowed-acls": [] },
  "app-privilege-capabilities": [],
  "issuer": "pki_internal"
}
EOF

# 9) 签 profile -> p7b
run sign-profile -mode localSign -keyAlias oh-profile-ca -keyPwd "$KEYPW" \
  -profileCertFile profile-cert.cer -inFile app-profile.json \
  -signAlg SHA256withECDSA -keystoreFile profile.jks -keystorePwd "$STOREPW" \
  -outFile mailcraft-debug.p7b

echo "=== 产物 ==="
ls -la "$OUT"/*.cer "$OUT"/*.p12 "$OUT"/*.p7b 2>&1 | tr -d '\r'
echo "=== 校验 p7b ==="
"$JAVA" -jar "$SIGTOOL" verify-profile -inFile "$OUT/mailcraft-debug.p7b" 2>&1 | grep -iE "verifiedPassed|message|bundle-name" | head

echo
echo "下一步：把 signing/ 下四件套路径填进 build-profile.json5 的 signingConfigs（storeFile/keyAlias/signAlg/certFile/profileFile），"
echo "再跑 scripts/build_mailcraft.sh 得到已签名 HAP，hdc install 到真机。"
