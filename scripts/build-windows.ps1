#!/usr/bin/env pwsh
# shareboard Windows 배포물 빌드 — 설치 파일(NSIS .exe) + MSI + 포터블 exe + sb-server.exe.
#
# Windows 에서 실행한다. Tauri 는 macOS/Linux → Windows 크로스 번들링을 지원하지 않으므로
# (WebView2 + MSVC + NSIS 필요) 다른 OS 에서는 CI(.github/workflows/build.yml 의 windows-bundle)를 쓴다.
#
#   pwsh -File scripts/build-windows.ps1
#
# 산출물: <repo>/release/
#   shareboard_<ver>_x64-setup.exe  설치 파일(현재 사용자 설치, 관리자 권한 불필요)
#   shareboard_<ver>_x64_en-US.msi  MSI(그룹 정책 배포용, 선택)
#   shareboard-portable.exe         설치 없이 바로 실행
#   sb-server.exe                   릴레이 서버 단독 바이너리
#
# 사전 준비: Rust(MSVC 툴체인) + VS Build Tools(C++), Node 22 + pnpm.

param(
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$OutDir = 'release',
    # CI 처럼 pnpm install 을 이미 했을 때.
    [switch]$SkipInstall,
    # NSIS 설치 파일만 만들고 MSI 는 건너뛴다.
    [switch]$NoMsi
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not $SkipInstall) {
        Write-Host '==> pnpm install' -ForegroundColor Cyan
        pnpm install
        if ($LASTEXITCODE -ne 0) { throw 'pnpm install 실패' }
    }

    # 프런트엔드(vite) 빌드는 tauri 의 beforeBuildCommand 가 수행한다.
    # `pnpm exec` 로 CLI 를 직접 호출한다(pnpm 이 --target 등을 삼키지 않도록).
    Write-Host "==> tauri build ($Target) — 설치 파일 + 포터블 exe" -ForegroundColor Cyan
    pnpm exec tauri build --target $Target --bundles nsis
    if ($LASTEXITCODE -ne 0) { throw 'tauri build (nsis) 실패' }

    if (-not $NoMsi) {
        # MSI 는 WiX 다운로드가 필요해 실패할 수 있다 — 설치 파일(NSIS)이 주 배포물이므로 경고만.
        Write-Host '==> tauri build — MSI' -ForegroundColor Cyan
        pnpm exec tauri build --target $Target --bundles msi
        if ($LASTEXITCODE -ne 0) { Write-Warning 'MSI 번들 실패 — NSIS 설치 파일만 배포합니다.' }
    }

    Write-Host '==> cargo build -p sb-server --release' -ForegroundColor Cyan
    cargo build -p sb-server --release --target $Target
    if ($LASTEXITCODE -ne 0) { throw 'sb-server 빌드 실패' }

    # --- 산출물 모으기 ---
    $appRel = Join-Path 'src-tauri/target' (Join-Path $Target 'release')
    $bundle = Join-Path $appRel 'bundle'

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    Remove-Item (Join-Path $OutDir '*') -Recurse -Force -ErrorAction SilentlyContinue

    $setup = @(Get-ChildItem (Join-Path $bundle 'nsis') -Filter '*-setup.exe' -ErrorAction SilentlyContinue)
    if ($setup.Count -eq 0) { throw "설치 파일을 찾지 못했습니다: $bundle\nsis" }
    $setup | ForEach-Object { Copy-Item $_.FullName $OutDir }

    @(Get-ChildItem (Join-Path $bundle 'msi') -Filter '*.msi' -ErrorAction SilentlyContinue) |
        ForEach-Object { Copy-Item $_.FullName $OutDir }

    Copy-Item (Join-Path $appRel 'shareboard.exe') (Join-Path $OutDir 'shareboard-portable.exe')
    Copy-Item (Join-Path 'target' (Join-Path $Target 'release/sb-server.exe')) $OutDir

    Write-Host "==> 산출물: $((Resolve-Path $OutDir).Path)" -ForegroundColor Green
    Get-ChildItem $OutDir |
        Select-Object Name, @{ n = 'MB'; e = { [math]::Round($_.Length / 1MB, 2) } } |
        Format-Table -AutoSize
}
finally {
    Pop-Location
}
