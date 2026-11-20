# ShrinkClip

Windows 트레이 앱. **Win+Shift+S** 등으로 클립보드에 복사된 스크린샷을 자동으로 최적화합니다.
macOS의 [Clop](https://github.com/FuzzyIdeas/Clop) 클립보드 최적화 동작을 Windows용으로 옮긴 것입니다.

- Rust + Win32 API 직접 호출, 단일 exe (~0.8 MB), 대기 시 메모리 수 MB
- 압축 엔진: [libimagequant](https://github.com/ImageOptim/libimagequant) (pngquant 엔진)

## 동작 (Clop 기본값과 동일)

1. 클립보드에 "그림만" 들어오면 감지 (Office/텍스트/파일 복사 등은 건드리지 않음)
2. pngquant 방식(품질 0–100, speed 4)으로 팔레트 압축, 메타데이터 제거
   - 결과가 원본보다 크면 원본 유지
3. `%LOCALAPPDATA%\ShrinkClip\images\`에 저장하고 클립보드에 **이미지 + 파일 + 경로 텍스트**를 넣음
   (원본은 `originals\`에 보관, 3일 뒤 자동 삭제)
4. 해상도 축소는 자동으로 하지 않음. 결과 직후 30초 동안 단축키 사용 가능:

| 단축키 | 동작 |
| --- | --- |
| `Ctrl+Shift+-` | 축소 100 → 75 → 50 → 40 → 30 → 20 → 10 % |
| `Ctrl+Shift+Z` | 원본 복원 |

## 트레이 메뉴

- 일시정지
- 파일 경로도 복사 (기본 켜짐. 일부 앱이 이미지 대신 경로 텍스트를 붙여넣으면 끄세요)
- Windows 시작 시 실행
- 작업 폴더 열기 / 종료

설정은 `HKCU\Software\ShrinkClip`에 저장됩니다.

## 설치

[Releases](../../releases)에서 `shrinkclip.exe`를 받아 실행하세요. 설치 과정은 없습니다.

## 빌드

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu   # 또는 MSVC Build Tools가 있으면 msvc
cargo build --release
```

`rust-toolchain.toml`이 GNU 툴체인을 지정합니다(MSVC Build Tools 없이 빌드 가능).

## 라이선스

GPL-3.0-or-later (libimagequant가 GPL-3.0). Clop 역시 GPL-3.0입니다.
