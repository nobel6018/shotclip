# ShotClip

<img src="assets/shotclip.png" width="96" alt="ShotClip icon">

Windows용 스크린샷 트레이 앱.

- **macOS식 캡처**: `Alt+Shift+3` 전체 화면, `Alt+Shift+4` 영역/창 선택
- **Shottr식 도구**: `Alt+Shift+5` 스크롤 캡처, `6` 텍스트 인식(OCR), `7` 색상 추출, `8` 픽셀 자
- **클립보드 최적화**: 캡처나 Win+Shift+S 등으로 클립보드에 들어온 스크린샷을 자동으로 압축합니다.
  macOS의 [Clop](https://github.com/FuzzyIdeas/Clop) 동작을 Windows용으로 옮긴 것입니다.

- **앱 안에서 업데이트**: 새 버전 알림을 누르면 받아서 다시 켜집니다
- Rust + Win32 API 직접 호출, 단일 exe (~1.1 MB), 대기 시 메모리 수 MB
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

## 도구 (Shottr 참고)

| 단축키 | 동작 |
| --- | --- |
| `Alt+Shift+5` | **스크롤 캡처**. 스크롤할 영역을 드래그한 뒤 휠로 내리면 이어 붙입니다. `Space` 자동 스크롤(페이지 끝에서 저절로 끝남), `Enter` 완료, `Esc` 취소 |
| `Alt+Shift+6` | **텍스트 인식**. 영역을 드래그하면 글자를 클립보드에 넣습니다 (Windows 내장 OCR, 한국어/영어, 오프라인) |
| `Alt+Shift+7` | **색상 추출**. 돋보기로 픽셀을 보고 클릭(또는 `Enter`)하면 `#RRGGBB`를 복사. 방향키로 1px 이동 |
| `Alt+Shift+8` | **픽셀 자**. 커서에서 같은 색이 끝나는 곳까지 가로/세로 거리 표시, 드래그하면 상자 크기. 클릭하면 `W × H` 복사(자는 계속 열림), 방향키 1px 이동 |

- 스크롤 캡처는 고정 헤더/푸터(스크롤해도 안 움직이는 줄)를 찾아 한 번만 넣습니다. 너무 빨리 스크롤해 이어 붙일 곳을 못 찾으면
  테두리가 빨갛게 바뀌니 조금 위로 되돌리면 이어집니다. 결과는 캡처와 똑같이 최적화·클립보드·바탕화면 저장을 거칩니다.
- 픽셀 자는 화면 배율이 100%가 아니면 100% 기준 크기도 함께 보여 줍니다.
- 모든 도구는 `Esc` / 우클릭으로 닫습니다.

## 클립보드 최적화 (Clop 기본값과 동일)

1. 클립보드에 "그림만" 들어오면 감지 (Office/텍스트/파일 복사 등은 건드리지 않음)
2. pngquant 방식(품질 0–100, speed 4)으로 팔레트 압축, 메타데이터 제거
   - 결과가 원본보다 크면 원본 유지
3. `%LOCALAPPDATA%\ShotClip\images\`에 저장하고 클립보드에 **이미지 + 파일 + 경로 텍스트**를 넣음
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
- 업데이트 확인 / 업데이트하고 다시 켜기
- 작업 폴더 열기 / 종료

설정은 `HKCU\Software\ShotClip`에 저장됩니다. (이전 이름 ShrinkClip의 설정과 시작 프로그램 등록은 자동으로 옮겨집니다.)

## 설치

[Releases](../../releases)에서 `shotclip.exe`를 받아 실행하세요. 설치 과정은 없습니다.

## 업데이트

0.3.0부터 앱이 스스로 업데이트합니다(0.2.0 이하는 한 번만 직접 받아 바꿔 주세요).
[sori](https://github.com/nobel6018/sori)의 Tauri updater와 같은 방식입니다.

1. 켤 때와 12시간마다 `releases/latest/download/latest.json`을 읽어 새 버전을 확인합니다 (트레이 메뉴 "업데이트 확인"으로 바로 확인)
2. 새 버전이 있으면 알림만 띄웁니다. 저절로 설치하지 않습니다
3. 알림이나 트레이 메뉴 "업데이트하고 다시 켜기"를 누르면 내려받고, **minisign 서명**을 앱에 박힌 공개 키로 확인한 뒤
   실행 파일을 바꾸고 다시 켭니다. 캡처 도구가 열려 있으면 끝낸 뒤 다시 누르라고 알립니다

기록은 `%LOCALAPPDATA%\ShotClip\update\update.log`에 남습니다.

| 항목 | 위치 |
| --- | --- |
| 공개 키 | `src/updater.rs` `PUBKEY` |
| 비밀 키 | 이 PC의 `~/.shotclip/updater.key` (비밀번호 없음). **잃으면 설치된 앱에 업데이트를 보낼 수 없으니 따로 백업** |

릴리스: `scripts/release.sh` — 빌드 → 서명 → `latest.json` 생성 → GitHub Release에 `shotclip.exe`, `shotclip.exe.minisig`, `latest.json` 업로드.

## 빌드

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu   # 또는 MSVC Build Tools가 있으면 msvc
cargo build --release
```

`rust-toolchain.toml`이 GNU 툴체인을 지정합니다(MSVC Build Tools 없이 빌드 가능).

아이콘은 `assets/shotclip.ico`이며 `build.rs`가 windres 없이 exe 리소스로 넣습니다.
아이콘을 다시 그리려면 `cargo run --example gen_icon`.

## 라이선스

GPL-3.0-or-later (libimagequant가 GPL-3.0). Clop 역시 GPL-3.0입니다.
