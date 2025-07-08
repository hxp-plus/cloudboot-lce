# CloudBoot Lite (Clientless Edition)

CloudBoot 精简无客户端版，一个及其简单的 PXE 装机软件。

## 声明

本项目由受云霁科技 CloudBoot 启发而来，与云霁科技 ClouudBoot 产品除名字里都带“CloudBoot”字样外无任何关联。项目开发过程中未参考 CloudBoot 代码，仅参考了互联网上公开的 iPXE 资料实现了物理机安装功能，云霁科技 CloudBoot 所拥有的其它功能均未实现。类似于 Linux 与 Unix 名字里都有 nix，Linux 受 Unix 启发且实现了 Unix 的功能，但 Linux 的开发未参考 Unix 代码。如本项目名称涉及侵权，请提 issue 联系本人修改。

## 部署指南

### 部署 dhcp 服务

安装 DHCP 服务：

```bash
yum install dhcp
```

配置 DHCP 服务监听装机网络网卡，首先复制配置文件到 `/etc` 目录：

```bash
cp /lib/systemd/system/dhcpd.service /etc/systemd/system/
```

修改 `/etc/systemd/system/dhcpd.service` ：

```ini
ExecStart=/usr/sbin/dhcpd -f -cf /etc/dhcp/dhcpd.conf -user dhcpd -group dhcpd --no-pid $DHCPDARGS ens36
```

配置装机网卡 IP 地址：

```bash
nmcli con add type ethernet ipv4.method manual ipv4.addresses 172.179.128.1/24 autoconnect yes con-name ens36 ifname ens36
```

修改完成后使 `systemd` 配置文件生效：

```bash
systemctl daemon-reload
```

其中 `ens36` 为网卡名需要客户化。新建 dhcp 服务配置 `/etc/dhcp/dhcpd.conf` ：

```conf
allow booting;
allow bootp;
ddns-update-style none;
ping-check true;
ping-timeout 3;
default-lease-time 60;
max-lease-time 60;
authoritative;
ignore-client-uids true;
deny duplicates;
next-server 172.179.128.1;
option domain-name "pxe";
option domain-name-servers 172.179.128.1;
option arch code 93 = unsigned integer 16;

if option arch = 00:00 {
    filename "undionly.kpxe";
} elsif option arch = 00:07 {
    filename "ipxex64.efi";
} elsif option arch = 00:0b {
    filename "ipxeaa64.efi";
}

subnet 172.179.128.0 netmask 255.255.255.0 {
    range 172.179.128.2 172.179.128.255;
    # option routers 192.168.1.1;
}
```

完成配置后设置服务开机自启动并重启服务：

```bash
systemctl enable dhcpd
systemctl restart dhcpd
```

### 部署 tftp 服务

安装 `tftp-server` 和 `xinetd` ：

```bash
yum install tftp-server xinetd
```

设置服务开机自启动并启动服务：

```bash
systemctl --now enable xinetd
systemctl --now enable tftp
```

编译 `iPXE` 固件并将其放到 tftp 目录：

```bash
yum install git gcc binutils make perl xz-devel mtools
git clone https://github.com/ipxe/ipxe.git
cd ipxe/src/
cat >boot.ipxe <<'EOF'
#!ipxe
echo Configure dhcp ....
dhcp
chain --replace http://osinstall.pxe/http-boot.ipxe
shell
EOF
make bin-x86_64-efi/ipxe.efi EMBED=boot.ipxe
cp bin-x86_64-efi/ipxe.efi /var/lib/tftpboot/ipxex64.efi
chmod 644 /var/lib/tftpboot/ipxex64.efi
```

### 部署 DNS 服务

安装 `dnsmasq` ：

```bash
yum install dnsmasq
```

修改配置文件 `/etc/dnsmasq.conf` ，添加如下配置（如默认配置有冲突则修改）：

```conf
interface=ens36
address=/osinstall/172.179.128.1
address=/osinstall.pxe/172.179.128.1
```

其中 `ens36` 为装机网络网卡需要客户化，设置服务开机自启动并启动服务：

```bash
systemctl --now enable dnsmasq
```

### 部署 nginx 服务

安装 `nginx` ：

```bash
yum install nginx
```

启动 `nginx` 服务：

```bash
systemctl enable --now nginx.service
```

新建 `iPXE` 配置 `/usr/share/nginx/html/http-boot.ipxe` ：

```ipxe
#!ipxe
echo Serial number: ${serial}
echo Build arch: ${buildarch}
echo MAC address: ${mac}
chain --replace http://osinstall.pxe/api/ipxe/${serial:uristring} || chain --replace http://osinstall.pxe/default-${buildarch}.ipxe || reboot --warm
```

修改其权限为 `644` ：

```bash
chmod 644 /usr/share/nginx/html/http-boot.ipxe
```

将 银河麒麟 V10SP4 装机 ISO 复制到 nginx 目录：

```bash
umask 0022
mkdir -p /usr/share/nginx/html/repo/kylin/v10sp4/
rsync -a /mnt/ /usr/share/nginx/html/repo/kylin/v10sp4/
```

创建 `x86_64` 架构默认 `iPXE` 文件 `/usr/share/nginx/html/default-x86_64.ipxe`：

```ipxe
#!ipxe
echo Booting ${serial}
kernel http://osinstall.pxe/repo/kylin/v10sp4/images/pxeboot/vmlinuz initrd=initrd.img ksdevice=bootif BOOTIF=01-${netX/mac:hexhyp} inst.sshd inst.repo=http://osinstall.pxe/repo/kylin/v10sp4 inst.text inst.ks=http://osinstall.pxe/default-kickstart.cfg
initrd http://osinstall.pxe/repo/kylin/v10sp4/images/pxeboot/initrd.img
boot
```

创建默认 `/usr/share/nginx/html/default-kickstart.cfg` 文件：

```kickstart
text
sshpw --username=root $6$dCZFWv.CPy9rrzpb$dvUdNFfzrVjaG99eqYLdMulOB.LegqE4CiND9SpuBw6LdJUoXCvmZChkKwNOqHYAthiine9U/nteCtDhrNXG1/ --iscrypted

%pre --interpreter=/bin/bash
echo 0 >/tmp/install-progress
sleep 30
while :;do
  curl -m 3 -s http://osinstall.pxe/api/ping -o /dev/null || reboot
  sleep 10
done
%end
```

其中 `sshpw` 后面的密码需要用 `openssl passwd -6` 生成密码的 hash 后填入。

新建 nginx 配置 `/etc/nginx/default.d/cloudboot-lce.conf` ：

```conf
location /api {
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_pass http://localhost:8000;
}
```

重启 nginx 并配置开机启动：

```bash
systemctl enable nginx
systemctl restart nginx
```

```
I need a rust web server using sqlite to act like a pxe install server. help me to bootstrap the project. the server does 3 things: 1. when /api/ipxe/serial is requested, look for the operating system in hosts table and retuen ipxe script defined in ipxe table. 2. backgroud task periodically scans jobs table to find new os install task and update hosts table. 3. ipxe table stores ipxe file paths for each system.
```

### 安装依赖

```bash
yum install sqlite-devel sshpass
```

## 修改 motd

新建文件 `/etc/motd.d/cloudboot-lce` :

```text
   ____ _                 _ ____              _     _     ____ _____
  / ___| | ___  _   _  __| | __ )  ___   ___ | |_  | |   / ___| ____|
 | |   | |/ _ \| | | |/ _` |  _ \ / _ \ / _ \| __| | |  | |   |  _|
 | |___| | (_) | |_| | (_| | |_) | (_) | (_) | |_  | |__| |___| |___
  \____|_|\___/ \__,_|\__,_|____/ \___/ \___/ \__| |_____\____|_____|

```

（该 ASCII Art 由 <https://patorjk.com/software/taag/#p=display&f=Standard&t=CloudBoot%20LCE> 生成）

## 导入 iPXE 文件

以麒麟 V10SP4 为例，新建 iPXE 文件 `Kylin-V10SP4-X86.ipxe` ：

```ipxe
#!ipxe
echo "Booting ${serial}"
kernel http://osinstall.pxe/repo/kylin/v10sp4/images/pxeboot/vmlinuz initrd=initrd.img ksdevice=bootif BOOTIF=01-${netX/mac:hexhyp} inst.sshd inst.repo=http://osinstall.pxe/repo/kylin/v10sp4/ inst.text inst.ks=http://osinstall.pxe/repo/kylin/v10sp4/ks-pxe.cfg
initrd http://osinstall.pxe/repo/kylin/v10sp4/images/pxeboot/initrd.img
boot
```

然后将其注册到数据库：

```bash
sqlite3 -cmd '.headers on' -cmd '.mode column' cloudboot-lce.db "insert into ipxe (os,script) values ('Kylin-V10SP4-X86','/root/cloudboot-lce/assets/Kylin-V10SP4-X86.ipxe');"
```

## 使用指南

### SQL 示例

找到所有已纳管主机：

```shell
sqlite3 -cmd '.headers on' -cmd '.mode column' cloudboot-lce.db 'SELECT * FROM hosts;'
```

找到所有操作系统和对应的 iPXE 文件：

```shell
sqlite3 -cmd '.headers on' -cmd '.mode tab' cloudboot-lce.db 'SELECT * FROM ipxe;'
```

### 调试指南

本项目使用 rust-1.88.0 ，对应 rustup 版本 1.28.2 ，下载地址：

- <https://static.rust-lang.org/rustup/archive/1.28.2/x86_64-unknown-linux-gnu/rustup-init>
- <https://static.rust-lang.org/dist/2025-06-26/rust-src-1.88.0.tar.xz>
- <https://static.rust-lang.org/dist/2025-06-26/rust-std-1.88.0-x86_64-unknown-linux-musl.tar.xz>
- <https://static.rust-lang.org/dist/2025-06-26/rust-1.88.0-x86_64-unknown-linux-gnu.tar.xz>
