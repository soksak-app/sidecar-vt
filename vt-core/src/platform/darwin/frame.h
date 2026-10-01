#ifndef SOKSAK_FRAME_H
#define SOKSAK_FRAME_H

#include <stdint.h>
#include <string.h>

// 터미널 글꼴. 설치된 글꼴 이름이나 시스템 고정폭 글꼴로 만든다.
struct FrameFont;
typedef struct FrameFont FrameFont;

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t cell_width;
    uint32_t cell_height;
    double font_size;
    const FrameFont *font; // 셀 메트릭을 계산하고 글자를 그리는 글꼴
} Metrics;

typedef struct {
    uint32_t col;
    uint32_t row;
    uint32_t width;
    const uint8_t *ch;
    uint32_t ch_len;
    uint8_t fg[3];
    uint8_t bg[3];
    uint8_t has_fg;
    uint8_t has_bg;
    uint8_t inverse;
    uint8_t underline;
} Cell;

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t cursor_col;
    uint32_t cursor_row;
    Cell *cells;
    uint32_t cell_count;
    uint8_t cursor_visible;
    uint8_t cursor_blink_visible;
    uint8_t cursor_shape; // 0 블록, 1 밑줄, 2 빔, 3 빈 블록, 4 숨김
    uint8_t default_foreground[3];
    uint8_t default_background[3];
    uint8_t default_cursor[3];
    uint32_t cursor_width; // 커서 위치 글자가 차지하는 칸 수. 넓은 글자는 2 다.
} Screen;

typedef struct {
    const uint8_t *data;
    uint32_t data_len;
    uint32_t x;
    uint32_t y;
    uint32_t width;
    uint32_t height;
    uint8_t preserve_aspect_ratio;
    uint8_t visible;
} InlineImageRaster;

// 불투명 Frame 타입. 구현 파일에서 정의한다
struct Frame;
typedef struct Frame Frame;

// 주어진 pixel 크기로 frame 을 만든다
// 실패하면 NULL 을 반환한다
Frame* frame_new(uint32_t width_px, uint32_t height_px);

// IOSurface ID (u32) 를 가져온다
uint32_t frame_id(Frame *frame);

// 16바이트 nonce 를 가져온다
// buf 는 최소 16바이트여야 한다
void frame_nonce(Frame *frame, uint8_t *buf);

// 화면을 frame 의 IOSurface 에 그린다
// Returns 0 on success, -1 on failure
int frame_draw(Frame *frame, Screen *screen, Metrics *metrics);
int frame_draw_with_inline_images(Frame *frame, Screen *screen, Metrics *metrics,
                                  InlineImageRaster *images, uint32_t image_count);

// frame 을 해제하고 IOSurface 를 release 한다
void frame_drop(Frame *frame);

// 사용자의 시스템 고정폭 글꼴을 만든다. 만들지 못하면 NULL 을 반환한다.
FrameFont *frame_font_system_monospace(void);
// 설치된 글꼴 가운데 family 이름이 정확히 같은 글꼴을 만든다. 없으면 NULL 을 반환한다.
FrameFont *frame_font_named(const char *family);
void frame_font_drop(FrameFont *font);
// 글꼴의 family 이름을 malloc 한 UTF-8 문자열로 반환한다. 호출자가 free 한다.
char *frame_font_family(const FrameFont *font);

// font 의 font_size(pt) 와 scale 로 셀 메트릭을 계산한다. 셀 폭은 'M' 의 advance 다.
Metrics frame_metrics(const FrameFont *font, double font_size, double scale);

// (x, y) 의 pixel 을 읽어 BGRA 값을 반환한다
// out 은 최소 4바이트여야 한다
// Returns 0 on success, -1 on failure
int frame_pixel(Frame *frame, uint32_t x, uint32_t y, uint8_t *out);

#endif // SOKSAK_FRAME_H
