#!/usr/bin/env bash
# 容器内一次性搭建端到端测试用 sshd（端口 2222，密码/密钥/agent 三种认证）
set -euo pipefail
cd "$(dirname "$0")"

SSHD_DIR=sshd_test
KEY_DIR=testkeys
PORT=2222
USER=tester
PASS=pass123

# 主机密钥
mkdir -p "$SSHD_DIR/empty" "$SSHD_DIR/run"
[ -f "$SSHD_DIR/host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$SSHD_DIR/host_ed25519"
[ -f "$SSHD_DIR/host_rsa" ] || ssh-keygen -q -t rsa -b 3072 -N '' -f "$SSHD_DIR/host_rsa"

# 客户端密钥（ed25519，无口令）；另一把仅用于 known_hosts 不匹配测试
mkdir -p "$KEY_DIR"
[ -f "$KEY_DIR/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEY_DIR/id_ed25519" -C lterm-e2e
[ -f "$KEY_DIR/id_other" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEY_DIR/id_other" -C lterm-other

# 测试用户
id "$USER" &>/dev/null || useradd -m "$USER"
echo "$USER:$PASS" | chpasswd
mkdir -p /home/$USER/.ssh
cp "$KEY_DIR/id_ed25519.pub" /home/$USER/.ssh/authorized_keys
chown -R $USER:$USER /home/$USER/.ssh
chmod 700 /home/$USER/.ssh; chmod 600 /home/$USER/.ssh/authorized_keys

cat > "$SSHD_DIR/sshd_config" <<EOF
Port $PORT
HostKey $PWD/$SSHD_DIR/host_ed25519
HostKey $PWD/$SSHD_DIR/host_rsa
PermitRootLogin no
PasswordAuthentication yes
PubkeyAuthentication yes
AuthorizedKeysFile /home/$USER/.ssh/authorized_keys
UsePAM no
PidFile $PWD/$SSHD_DIR/run/sshd.pid
StrictModes no
PrintMotd no
AcceptEnv LANG LC_*
Subsystem sftp /usr/lib/openssh/sftp-server
EOF

# 停旧起新
if [ -f "$SSHD_DIR/run/sshd.pid" ]; then kill "$(cat "$SSHD_DIR/run/sshd.pid")" 2>/dev/null || true; fi
/usr/sbin/sshd -f "$SSHD_DIR/sshd_config" -E "$SSHD_DIR/sshd.log"
echo "sshd ok on :$PORT"

# ssh-agent（供 agent 认证测试）
if [ -f agent.env ] && SSH_AUTH_SOCK=$(grep -oP "SSH_AUTH_SOCK=\K.*" agent.env) ssh-add -l >/dev/null 2>&1; then
  echo "ssh-agent ok"
else
  rm -f agent.env agent.pid
  eval "$(ssh-agent -s)" >/dev/null
  echo "SSH_AUTH_SOCK=$SSH_AUTH_SOCK" > agent.env
  echo "SSH_AGENT_PID=$SSH_AGENT_PID" > agent.pid
  SSH_AUTH_SOCK="$SSH_AUTH_SOCK" ssh-add "$PWD/$KEY_DIR/id_ed25519"
  echo "ssh-agent ok"
fi

echo "LTERM_TEST_SSHD=127.0.0.1:$PORT"
echo "LTERM_TEST_KEY=$PWD/$KEY_DIR/id_ed25519"
echo "LTERM_TEST_USER=$USER LTERM_TEST_PASS=$PASS"
