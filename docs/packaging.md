# Arch 打包与安装验证


## 构建测试包

从维护工作区的 `packaging/arch/` 执行。该目录的打包文件单独维护，源码检出只提供应用源码和本文。脚本依赖仓库根目录和 `v$pkgver` 标签。先确认标签指向要验证的提交。

```sh
cd packaging/arch
./release.sh
SRCDEST="$PWD" makepkg -f
```

`release.sh` 调用 `make-source.sh` 从标签生成源码包，更新 `PKGBUILD` 的校验和并生成 `.SRCINFO`。`SRCDEST="$PWD"` 让 makepkg 使用本地生成的归档，发布后还要从 release 的下载地址复验。发布的详细维护记录在打包工作区的 `packaging-notes.md`。

## 安装和升级验证

征得同意后用 `pacman -U` 安装实际生成的包文件。升级后按确认流程重启系统单元 `shisad.service` 和用户单元 `shisa-agent.service`，再重新打开设置窗口。首次安装需要设置独立的应用锁密码，步骤见 [README](../README.md#安装)。用 `pacman -Qkk shisa` 核对已安装文件，然后按[测试与验收](testing.md)核查改动涉及的行为。

2026-11-17 用临时 Arch 包替换受保护文件，确认 pacman 的事务后 hook 重新标记了新 inode，且在 30 秒定时核对前完成。2026-11-18 用 `extra-x86_64-build` 在干净 chroot 中完成构建。
