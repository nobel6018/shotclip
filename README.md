# ShrinkClip

<img src="assets/shrinkclip.png" width="96" alt="ShrinkClip icon">

Windows용 스크린샷 트레이 앱.

- **macOS식 캡처**: `Alt+Shift+3` 전체 화면, `Alt+Shift+4` 영역/창 선택
- **클립보드 최적화**: 캡처나 Win+Shift+S 등으로 클립보드에 들어온 스크린샷을 자동으로 압축합니다.
  macOS의 [Clop](https://github.com/FuzzyIdeas/Clop) 동작을 Windows용으로 옮긴 것입니다.

- Rust + Win32 API 직접 호출, 단일 exe (~0.8 MB), 대기 시 메모리 수 MB
- 압축 엔진: [libimagequant](https://github.com/ImageOptim/libimagequant) (pngquant 엔진)

## 캡처 (macOS와 동일한 조작)

| 단축키 | 동작 |
| --- | --- |
| `Alt+Shift+3` | 전체 화면. 모니터마다 한 장씩 저장, 마우스가 있는 모니터를 클립보드에 |
| `Alt+Shift+4` | 화면이 멈추고 바로 **드래그**하면 직사각형 선택 (커서 옆에 크기 표시) |
| ↳ `Space` | 창 선택 모드 전환: 마우스를 올린 창이 강조되고 **클릭**하면 그 창만 캡처 |
| ↳ `Esc` / 우클릭 | 취소 |

- 창 캡처는 다른 창에 가려진 부분도 그 창 내용으로 찍습니다(PrintWindow). 렌더링이 안 되는 앱은 화면에서 잘라냅니다.
  Windows 11 둥근 모서리 바깥은 투명 처리됩니다.
- 결과는 아래 최적화를 거쳐 클립보드에 들어가고, 바탕화면에 `스크린샷 YYYY-MM-DD 오후 h.mm.ss.png`로 저장됩니다
  (트레이 메뉴에서 끌 수 있음). 30초 축소/복원 단축키도 그대로 쓸 수 있습니다.

## 클립보드 최적화 (Clop 기본값과 동일)

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
- 캡처를 바탕화면에도 저장 (기본 켜짐)
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

아이콘은 `assets/shrinkclip.ico`이며 `build.rs`가 windres 없이 exe 리소스로 넣습니다.
아이콘을 다시 그리려면 `cargo run --example gen_icon`.

## 라이선스

GPL-3.0-or-later (libimagequant가 GPL-3.0). Clop 역시 GPL-3.0입니다.
