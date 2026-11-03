#import <Foundation/Foundation.h>
#import <IOSurface/IOSurface.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>
#import <ImageIO/ImageIO.h>
#include "frame.h"
#include <stdio.h>
#include <string.h>

// 글꼴은 CoreText 글꼴 설명 하나를 소유한다.
struct FrameFont {
    CTFontDescriptorRef descriptor;
};

// A surface that a frame draws into. The window server marks a surface in use while it reads it, and a frame draws only
// into a surface that is not in use.
typedef struct {
    IOSurfaceRef surface;
    uint32_t id;
    uint8_t nonce[16];
} FrameTarget;

struct Frame {
    // The target that the last draw drew into: its surface, identifier and nonce. The targets own these values.
    IOSurfaceRef surface;
    uint32_t surface_id;
    uint8_t nonce[16];
    uint32_t width;
    uint32_t height;
    // The strips of a strip frame: a surface of the frame size that holds row r at the rows of row r, the
    // serialized cells of each row as it was drawn (a retained NSMutableArray of NSData) and the configuration they were
    // drawn with (a retained NSData), and the count of strips that the last draw drew.
    IOSurfaceRef strips;
    // The overlay of a strip frame: a surface of the frame size into which the CoreGraphics draw composites the inline
    // images over a copy of the strip pixels under them.
    IOSurfaceRef overlay;
    void *rows;
    void *configuration;
    uint32_t strips_drawn;
    FrameTarget *targets;
    uint32_t target_count;
    uint32_t current;
};

// 이웃한 셀은 대개 같은 색이다. 마지막으로 만든 색을 RGB 값으로 기억해 셀마다 색을 새로 만들지 않는다.
typedef struct {
    int valid;
    uint8_t rgb[3];
    CGColorRef color;
} ColorCache;

static CGColorRef cached_color(ColorCache *cache, const uint8_t rgb[3]) {
    if (cache->valid && memcmp(cache->rgb, rgb, 3) == 0) return cache->color;
    if (cache->color) CGColorRelease(cache->color);
    cache->color = CGColorCreateSRGB(rgb[0] / 255.0, rgb[1] / 255.0, rgb[2] / 255.0, 1.0);
    memcpy(cache->rgb, rgb, 3);
    cache->valid = cache->color != NULL;
    return cache->color;
}

// 셀 글자가 유니코드 스칼라 하나이고 기본 글꼴에 그 글리프가 있으면 글리프를 돌려준다. 결합 문자나
// 기본 글꼴에 없는 글자는 0 을 돌려주며, 그런 셀은 대체 글꼴을 고르는 CoreText 줄로 그린다.
static CGGlyph single_glyph(CTFontRef font, const uint8_t *bytes, uint32_t length) {
    uint32_t scalar;
    uint32_t used;
    if (length == 0) return 0;
    if (bytes[0] < 0x80) { scalar = bytes[0]; used = 1; }
    else if ((bytes[0] & 0xE0) == 0xC0 && length >= 2) { scalar = ((bytes[0] & 0x1F) << 6) | (bytes[1] & 0x3F); used = 2; }
    else if ((bytes[0] & 0xF0) == 0xE0 && length >= 3) { scalar = ((bytes[0] & 0x0F) << 12) | ((bytes[1] & 0x3F) << 6) | (bytes[2] & 0x3F); used = 3; }
    else if ((bytes[0] & 0xF8) == 0xF0 && length >= 4) {
        scalar = ((bytes[0] & 0x07) << 18) | ((bytes[1] & 0x3F) << 12) | ((bytes[2] & 0x3F) << 6) | (bytes[3] & 0x3F); used = 4;
    } else return 0;
    if (used != length) return 0;
    UniChar units[2];
    CFIndex count;
    if (scalar < 0x10000) { units[0] = (UniChar)scalar; count = 1; }
    else {
        uint32_t value = scalar - 0x10000;
        units[0] = (UniChar)(0xD800 + (value >> 10));
        units[1] = (UniChar)(0xDC00 + (value & 0x3FF));
        count = 2;
    }
    CGGlyph glyphs[2] = {0, 0};
    if (!CTFontGetGlyphsForCharacters(font, units, glyphs, count)) return 0;
    return glyphs[0];
}

#ifdef SOKSAK_DIAGNOSTICS
static void store_cell_bounds(RenderCell *cell, CGRect bounds) {
    if (!cell || CGRectIsNull(bounds) || CGRectIsEmpty(bounds)) return;
    cell->has_ink = 1;
    cell->ink_x = CGRectGetMinX(bounds);
    cell->ink_y = CGRectGetMinY(bounds);
    cell->ink_width = CGRectGetWidth(bounds);
    cell->ink_height = CGRectGetHeight(bounds);
}
#endif

// 주어진 pixel 크기로 frame 을 만든다
// Creates a global BGRA surface in sRGB that carries its nonce as the soksak.frame property.
static IOSurfaceRef surface_create(uint32_t width_px, uint32_t height_px, uint8_t nonce[16]) {
    // BGRA 형식 코드: 0x42475241
    NSDictionary *properties = @{
        (id)kIOSurfaceWidth: @(width_px),
        (id)kIOSurfaceHeight: @(height_px),
        (id)kIOSurfacePixelFormat: @(0x42475241),  // 'BGRA'
        (id)kIOSurfaceBytesPerElement: @4,
// kIOSurfaceIsGlobal 은 deprecated 이지만 설계상 필요하다
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        (id)kIOSurfaceIsGlobal: @YES,
#pragma clang diagnostic pop
    };
    IOSurfaceRef surface = IOSurfaceCreate((CFDictionaryRef)properties);
    if (!surface) return NULL;

    // surface 에 sRGB 색 공간을 붙인다
    CGColorSpaceRef srgb_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    if (srgb_space) {
        CFPropertyListRef color_space_plist = CGColorSpaceCopyPropertyList(srgb_space);
        if (color_space_plist) {
            IOSurfaceSetValue(surface, kIOSurfaceColorSpace, color_space_plist);
            CFRelease(color_space_plist);
        }
        CGColorSpaceRelease(srgb_space);
    }

    // 16바이트 nonce 를 생성해 surface 의 property 로 설정한다
    arc4random_buf(nonce, 16);
    NSData *nonce_data = [NSData dataWithBytes:nonce length:16];
    IOSurfaceSetValue(surface, CFSTR("soksak.frame"), (CFDataRef)nonce_data);
    return surface;
}

static void frame_use_target(Frame *frame, uint32_t index) {
    FrameTarget *target = &frame->targets[index];
    frame->current = index;
    frame->surface = target->surface;
    frame->surface_id = target->id;
    memcpy(frame->nonce, target->nonce, 16);
}

static Frame* frame_create(uint32_t width_px, uint32_t height_px) {
    Frame *frame = calloc(1, sizeof(Frame));
    if (!frame) return NULL;
    frame->width = width_px;
    frame->height = height_px;
    frame->targets = calloc(1, sizeof(FrameTarget));
    if (!frame->targets) {
        free(frame);
        return NULL;
    }
    FrameTarget *target = &frame->targets[0];
    target->surface = surface_create(width_px, height_px, target->nonce);
    if (!target->surface) {
        free(frame->targets);
        free(frame);
        return NULL;
    }
    target->id = IOSurfaceGetID(target->surface);
    frame->target_count = 1;
    frame_use_target(frame, 0);
    return frame;
}

Frame* frame_new(uint32_t width_px, uint32_t height_px) {
    return frame_create(width_px, height_px);
}

// Creates a frame that keeps a strip per screen row and composes each draw on the CPU. A GPU pass is not used: its only work
// is this copy and the rectangles of the cursor and the images, and the Metal driver keeps at least 70 MB for a process
// that encodes any pass, while the copy costs less than a millisecond per frame (S1-13).
Frame* frame_new_strip(uint32_t width_px, uint32_t height_px) {
    Frame *frame = frame_create(width_px, height_px);
    if (!frame) return NULL;
    uint8_t unused_nonce[16];
    frame->strips = surface_create(width_px, height_px, unused_nonce);
    frame->overlay = surface_create(width_px, height_px, unused_nonce);
    if (!frame->strips || !frame->overlay) {
        frame_drop(frame);
        return NULL;
    }
    frame->rows = (__bridge_retained void *)[NSMutableArray array];
    return frame;
}

// A rectangle of pixels that a draw changes over the strips, in the order of the array: the overlay under an image, the
// color of a fill, or one minus the pixel for a difference fill, as kCGBlendModeDifference with white gives.
typedef enum { COMPOSE_OVERLAY, COMPOSE_FILL, COMPOSE_DIFFERENCE } ComposeKind;

typedef struct {
    uint32_t left;
    uint32_t top;
    uint32_t right;
    uint32_t bottom;
    uint8_t bgr[3];
    ComposeKind kind;
} ComposeRect;

// Appends a rectangle given in the bottom-left coordinates of the CoreGraphics draw. It covers the pixels whose centers it
// contains, on its left and top edges but not on its right and bottom edges, and is clamped to the frame.
static void compose_append(NSMutableData *rects, const Frame *frame, CGRect rect, const uint8_t rgb[3], ComposeKind kind) {
    CGFloat top = (CGFloat)frame->height - rect.origin.y - rect.size.height;
    ComposeRect item = {
        .left = (uint32_t)MAX(ceil(rect.origin.x - 0.5), 0.0),
        .top = (uint32_t)MAX(ceil(top - 0.5), 0.0),
        .right = (uint32_t)MIN(MAX(ceil(rect.origin.x + rect.size.width - 0.5), 0.0), (CGFloat)frame->width),
        .bottom = (uint32_t)MIN(MAX(ceil(top + rect.size.height - 0.5), 0.0), (CGFloat)frame->height),
        .bgr = {rgb[2], rgb[1], rgb[0]},
        .kind = kind,
    };
    [rects appendBytes:&item length:sizeof(item)];
}

static int frame_draw_strips(Frame *frame, Screen *screen, Metrics *metrics, InlineImageRaster *images, uint32_t image_count,
                          RenderOutput *render_output);

// Decodes the first image of the data with ImageIO, or returns NULL when the data is missing or does not decode.
static CGImageRef image_decode(const uint8_t *data, size_t length) {
    if (!data || length == 0) return NULL;
    CFDataRef image_data = CFDataCreate(NULL, data, length);
    CGImageSourceRef source = image_data ? CGImageSourceCreateWithData(image_data, NULL) : NULL;
    CGImageRef image = source ? CGImageSourceCreateImageAtIndex(source, 0, NULL) : NULL;
    if (image_data) CFRelease(image_data);
    if (source) CFRelease(source);
    return image;
}

// The size at which a frame draws an image. False when the image has no area at that size.
static bool inline_image_drawn_size(CGImageRef image, uint32_t requested_width, uint32_t requested_height,
                                    uint8_t preserve_aspect_ratio, CGFloat *drawn_width, CGFloat *drawn_height) {
    CGFloat natural_width = (CGFloat)CGImageGetWidth(image);
    CGFloat natural_height = (CGFloat)CGImageGetHeight(image);
    CGFloat width = requested_width ? requested_width : natural_width;
    CGFloat height = requested_height ? requested_height : natural_height;
    if (preserve_aspect_ratio) {
        if (requested_width && !requested_height) {
            height = width * natural_height / natural_width;
        } else if (!requested_width && requested_height) {
            width = height * natural_width / natural_height;
        }
    }
    *drawn_width = width;
    *drawn_height = height;
    return width > 0 && height > 0;
}

// Gives the height at which a frame draws an image. The inline image store calls it when it keeps an image, so an image
// that does not decode is rejected before any frame draws it.
int frame_inline_image_drawn_height(const uint8_t *data, size_t length, uint32_t width, uint32_t height,
                                    uint8_t preserve_aspect_ratio, uint32_t *drawn_height) {
    CGImageRef image = image_decode(data, length);
    if (!image) return 0;
    CGFloat size[2] = {0, 0};
    bool drawable = inline_image_drawn_size(image, width, height, preserve_aspect_ratio, &size[0], &size[1]);
    CGImageRelease(image);
    if (!drawable) return 0;
    *drawn_height = (uint32_t)ceil(size[1]);
    return 1;
}

// An image whose origin is past the frame has no pixel to draw. Both draws skip it.
static bool inline_image_outside(const Frame *frame, const InlineImageRaster *raster) {
    return raster->x >= frame->width || (int64_t)raster->y >= (int64_t)frame->height;
}

// Decodes an inline image and gives the size at which the frame draws it, or returns NULL when the data is missing or does
// not decode. The drawing context clips the part past the frame. Both draws use it, so they refuse and size the same images.
static CGImageRef inline_image_decode(const InlineImageRaster *raster, CGFloat *drawn_width, CGFloat *drawn_height) {
    CGImageRef image = image_decode(raster->data, raster->data_len);
    if (!image) return NULL;
    if (!inline_image_drawn_size(image, raster->width, raster->height, raster->preserve_aspect_ratio, drawn_width,
                                 drawn_height)) {
        CGImageRelease(image);
        return NULL;
    }
    return image;
}

uint32_t frame_id(Frame *frame) {
    if (!frame) return 0;
    return frame->surface_id;
}

void frame_nonce(Frame *frame, uint8_t *buf) {
    if (frame && buf) {
        memcpy(buf, frame->nonce, 16);
    }
}

int frame_draw(Frame *frame, Screen *screen, Metrics *metrics) {
    return frame_draw_with_inline_images(frame, screen, metrics, NULL, 0, NULL);
}

// Draws the screen with CoreGraphics into the surface, which has the size of the frame. Without a row selection it draws
// the whole frame: the background, every cell, the inline images and the cursor. With a selection (one flag per screen row)
// it draws only the selected rows, each from a fill of its row box with the default background, as the strips of a strip
// frame; the frame then draws the images and the cursor itself.
static int frame_draw_cg(Frame *frame, IOSurfaceRef surface, Screen *screen, Metrics *metrics,
                         InlineImageRaster *images, uint32_t image_count,
                         RenderOutput *render_output, const uint8_t *rows) {
#ifdef SOKSAK_DIAGNOSTICS
    if (render_output && (!render_output->cells || render_output->cell_capacity < screen->cell_count)) return -1;
#else
    (void)render_output;
#endif

    // surface 를 잠근다
    IOSurfaceLock(surface, 0, NULL);

    // surface 데이터를 가져온다
    void *base_address = IOSurfaceGetBaseAddress(surface);
    if (!base_address) {
        IOSurfaceUnlock(surface, 0, NULL);
        return -1;
    }

    size_t bytes_per_row = IOSurfaceGetBytesPerRow(surface);
#ifdef SOKSAK_DIAGNOSTICS
    if (render_output) {
        render_output->cell_count = screen->cell_count;
        render_output->bytes_per_row = bytes_per_row;
    }
#endif

    // sRGB 색 공간을 만든다
    CGColorSpaceRef color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    if (!color_space) {
        IOSurfaceUnlock(surface, 0, NULL);
        return -1;
    }

    // sRGB 를 사용하는 BGRA8 bitmap context 를 만든다
    CGContextRef ctx = CGBitmapContextCreateWithData(
        base_address,
        frame->width, frame->height,
        8,  // 성분당 비트 수
        bytes_per_row,
        color_space,
        kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
        NULL,  // release 콜백
        NULL   // release 콜백 정보
    );
    CGColorSpaceRelease(color_space);

    if (!ctx) {
        IOSurfaceUnlock(surface, 0, NULL);
        return -1;
    }

    // frame_metrics 에서 사용하는 것과 같은 크기로 글꼴을 설정한다
    CTFontRef font = metrics->font ? CTFontCreateWithFontDescriptor(metrics->font->descriptor, metrics->font_size, NULL) : NULL;
    if (!font) {
        CGContextRelease(ctx);
        IOSurfaceUnlock(surface, 0, NULL);
        return -1;
    }

    // 기본 색상은 Rust 엔진과 동일한 팔레트를 호출 단위로 전달받는다.
    CGColorRef default_fg_color = CGColorCreateSRGB(
        screen->default_foreground[0] / 255.0,
        screen->default_foreground[1] / 255.0,
        screen->default_foreground[2] / 255.0,
        1.0);
    CGColorRef default_bg_color = CGColorCreateSRGB(
        screen->default_background[0] / 255.0,
        screen->default_background[1] / 255.0,
        screen->default_background[2] / 255.0,
        1.0);

    // CGContextFillRect 로 배경 전체를 채운다
    CGContextSetFillColorWithColor(ctx, default_bg_color);
    if (!rows) {
        CGContextFillRect(ctx, CGRectMake(0, 0, (CGFloat)frame->width, (CGFloat)frame->height));
    } else {
        for (uint32_t row = 0; row < screen->height; row++) {
            if (!rows[row]) continue;
            CGContextFillRect(ctx, CGRectMake(0, (CGFloat)frame->height - ((CGFloat)row + 1.0) * metrics->cell_height,
                                              (CGFloat)frame->width, metrics->cell_height));
        }
        // The margin below the rows holds the default background, so the strips cover the whole frame.
        CGFloat rows_height = (CGFloat)screen->height * metrics->cell_height;
        if (rows_height < (CGFloat)frame->height) {
            CGContextFillRect(ctx, CGRectMake(0, 0, (CGFloat)frame->width, (CGFloat)frame->height - rows_height));
        }
    }

    ColorCache fg_cache = {0};
    ColorCache bg_cache = {0};
    CGFloat descent = CTFontGetDescent(font);
#ifdef SOKSAK_DIAGNOSTICS
    if (render_output) {
        render_output->ascent = CTFontGetAscent(font);
        render_output->descent = descent;
        render_output->leading = CTFontGetLeading(font);
        render_output->underline_position = CTFontGetUnderlinePosition(font);
        render_output->underline_thickness = CTFontGetUnderlineThickness(font);
    }
#endif
    // Each row is clipped to its row box, so ink that extends above or below its row stays in that row; the strip frame
    // draws the rows as separate strips and shows the same pixels.
    int64_t clipped_row = -1;
    // 셀을 그린다
    for (uint32_t i = 0; i < screen->cell_count; i++) {
        Cell *cell = &screen->cells[i];
        if (rows && (cell->row >= screen->height || !rows[cell->row])) continue;
        if ((int64_t)cell->row != clipped_row) {
            if (clipped_row >= 0) CGContextRestoreGState(ctx);
            CGContextSaveGState(ctx);
            CGContextClipToRect(ctx, CGRectMake(0, (CGFloat)frame->height - ((CGFloat)cell->row + 1.0) * metrics->cell_height,
                                                (CGFloat)frame->width, metrics->cell_height));
            clipped_row = cell->row;
        }
#ifdef SOKSAK_DIAGNOSTICS
        RenderCell *render_cell = NULL;
        if (render_output) {
            render_cell = &render_output->cells[i];
            memset(render_cell, 0, sizeof(*render_cell));
            render_cell->row = cell->row;
            render_cell->col = cell->col;
            render_cell->width = cell->width;
        }
#endif

        // Core Graphics의 하단 원점을 터미널 행 좌표로 변환한다.
        CGFloat x = cell->col * metrics->cell_width;
        CGFloat y = (CGFloat)frame->height - ((CGFloat)cell->row + 1.0) * metrics->cell_height;

        // 색은 이웃 셀과 같으면 다시 만들지 않는다.
        CGColorRef bg_color = cell->has_bg ? cached_color(&bg_cache, cell->bg) : NULL;
        CGColorRef fg_color = cell->has_fg ? cached_color(&fg_cache, cell->fg) : NULL;
        if (!bg_color) bg_color = default_bg_color;
        if (!fg_color) fg_color = default_fg_color;

        // 반전 표시(inverse video)를 처리한다
        if (cell->inverse) {
            CGColorRef tmp = bg_color;
            bg_color = fg_color;
            fg_color = tmp;
        }
#ifdef SOKSAK_DIAGNOSTICS
        if (render_cell) {
            const uint8_t *resolved_foreground = cell->has_fg ? cell->fg : screen->default_foreground;
            const uint8_t *resolved_background = cell->has_bg ? cell->bg : screen->default_background;
            if (cell->inverse) {
                const uint8_t *tmp = resolved_background;
                resolved_background = resolved_foreground;
                resolved_foreground = tmp;
            }
            memcpy(render_cell->foreground, resolved_foreground, sizeof(render_cell->foreground));
            memcpy(render_cell->background, resolved_background, sizeof(render_cell->background));
        }
#endif

        // 기본 배경은 이미 칠했다.
        if (bg_color != default_bg_color) {
            CGContextSetFillColorWithColor(ctx, bg_color);
            CGContextFillRect(ctx, CGRectMake(x, y, metrics->cell_width * cell->width, metrics->cell_height));
        }

        CGGlyph glyph = (cell->ch_len > 0 && cell->width > 0 && cell->ch) ? single_glyph(font, cell->ch, cell->ch_len) : 0;
        if (glyph != 0) {
#ifdef SOKSAK_DIAGNOSTICS
            if (render_cell) render_cell->font_run_count = 1;
#endif
            // 기본 글꼴의 글리프 하나는 줄 객체 없이 그린다. 넓은 글자는 CoreText 줄과 같이 두 칸 가운데에 둔다.
            CGFloat offset = 0;
            if (cell->width == 2) {
                CGSize advance;
                CTFontGetAdvancesForGlyphs(font, kCTFontOrientationHorizontal, &glyph, &advance, 1);
                CGFloat span = metrics->cell_width * 2;
                if (advance.width < span) offset = (span - advance.width) / 2;
            }
            CGPoint position = CGPointMake(x + offset, y + descent);
            CGContextSetFillColorWithColor(ctx, fg_color);
            // 글리프 위치는 텍스트 행렬을 거친다. 대체 경로의 CGContextSetTextPosition 이 옮긴 행렬을 되돌린다.
            CGContextSetTextMatrix(ctx, CGAffineTransformIdentity);
#ifdef SOKSAK_DIAGNOSTICS
            CGPathRef glyph_path = render_cell ? CTFontCreatePathForGlyph(font, glyph, NULL) : NULL;
            if (render_cell && glyph_path) {
                CGRect glyph_bounds = CGPathGetPathBoundingBox(glyph_path);
                glyph_bounds.origin.x += position.x;
                glyph_bounds.origin.y += position.y;
                store_cell_bounds(render_cell, glyph_bounds);
                CGPathRelease(glyph_path);
            }
#endif
            CTFontDrawGlyphs(font, &glyph, &position, 1, ctx);
        } else if (cell->ch_len > 0 && cell->width > 0) {
            if (!cell->ch) {
                if (fg_cache.color) CGColorRelease(fg_cache.color);
                if (bg_cache.color) CGColorRelease(bg_cache.color);
                CFRelease(font);
                CGContextRelease(ctx);
                CGColorRelease(default_fg_color);
                CGColorRelease(default_bg_color);
                IOSurfaceUnlock(surface, 0, NULL);
                return -1;
            }
            NSString *ch = [[NSString alloc] initWithBytes:cell->ch length:cell->ch_len encoding:NSUTF8StringEncoding];
            if (!ch) {
                if (fg_cache.color) CGColorRelease(fg_cache.color);
                if (bg_cache.color) CGColorRelease(bg_cache.color);
                CFRelease(font);
                CGContextRelease(ctx);
                CGColorRelease(default_fg_color);
                CGColorRelease(default_bg_color);
                IOSurfaceUnlock(surface, 0, NULL);
                return -1;
            }
            {
                CFStringRef cf_str = (__bridge CFStringRef)ch;

                // 글꼴과 전경색으로 attributes dictionary 를 만든다
                CGFloat descent = CTFontGetDescent(font);
                NSDictionary *attrs = @{
                    (id)kCTFontAttributeName: (__bridge id)font,
                    (id)kCTForegroundColorAttributeName: (__bridge id)fg_color
                };
                CFAttributedStringRef attr_str = CFAttributedStringCreate(NULL, cf_str, (__bridge CFDictionaryRef)attrs);

                if (attr_str) {
                    CTLineRef line = CTLineCreateWithAttributedString(attr_str);
                    if (line) {
#ifdef SOKSAK_DIAGNOSTICS
                        if (render_cell) {
                            CFArrayRef runs = CTLineGetGlyphRuns(line);
                            render_cell->font_run_count = (uint32_t)CFArrayGetCount(runs);
                        }
#endif
                        // 넓은 글자의 폭이 두 칸보다 좁으면(대체 글꼴의 한글 등) 두 칸 안에서 가로 가운데에 둔다.
                        // 글자 크기는 바꾸지 않는다. 두 칸보다 넓은 글자는 칸의 시작에 둔다.
                        CGFloat offset = 0;
                        if (cell->width == 2) {
                            CGFloat advance = CTLineGetTypographicBounds(line, NULL, NULL, NULL);
                            CGFloat span = metrics->cell_width * 2;
                            if (advance < span) offset = (span - advance) / 2;
                        }
                        CGContextSetTextPosition(ctx, x + offset, y + descent);
#ifdef SOKSAK_DIAGNOSTICS
                        if (render_cell) {
                            CGRect line_bounds = CTLineGetImageBounds(line, ctx);
                            store_cell_bounds(render_cell, line_bounds);
                        }
#endif
                        CTLineDraw(line, ctx);
                        CFRelease(line);
                    }
                    CFRelease(attr_str);
                }
            }
        }

        // 밑줄은 글꼴의 밑줄 위치와 두께로 셀 폭 전체에 그린다. 두께는 한 픽셀보다 얇지 않다.
        if (cell->underline && cell->width > 0) {
            CGFloat thickness = MAX(CTFontGetUnderlineThickness(font), 1.0);
            CGFloat baseline = y + CTFontGetDescent(font);
            CGFloat top = baseline + CTFontGetUnderlinePosition(font) - thickness / 2;
            CGRect underline_rect = CGRectMake(x, MAX(top, y), metrics->cell_width * cell->width, thickness);
#ifdef SOKSAK_DIAGNOSTICS
            if (render_cell) {
                render_cell->has_underline = 1;
                render_cell->underline_x = CGRectGetMinX(underline_rect);
                render_cell->underline_y = CGRectGetMinY(underline_rect);
                render_cell->underline_width = CGRectGetWidth(underline_rect);
                render_cell->underline_height = CGRectGetHeight(underline_rect);
            }
#endif
            CGContextSetFillColorWithColor(ctx, fg_color);
            CGContextFillRect(ctx, underline_rect);
        }

    }
    if (clipped_row >= 0) CGContextRestoreGState(ctx);
    if (fg_cache.color) CGColorRelease(fg_cache.color);
    if (bg_cache.color) CGColorRelease(bg_cache.color);

    for (uint32_t i = 0; !rows && i < image_count; i++) {
        InlineImageRaster *raster = &images[i];
        if (!raster->visible || inline_image_outside(frame, raster)) continue;
        CGFloat width = 0;
        CGFloat height = 0;
        CGImageRef image = inline_image_decode(raster, &width, &height);
        if (!image) {
            CFRelease(font);
            CGContextRelease(ctx);
            CGColorRelease(default_fg_color);
            CGColorRelease(default_bg_color);
            IOSurfaceUnlock(surface, 0, NULL);
            return -1;
        }
        CGContextDrawImage(ctx, CGRectMake(raster->x, (CGFloat)frame->height - raster->y - height, width, height), image);
        CGImageRelease(image);
    }

    // 커서 깜박임과 모양은 상위 서비스가 계산한 한 프레임 상태만 그린다.
    // 이 함수는 타이머나 애니메이션을 만들지 않으며, 화면 경계를 벗어난 커서는 버리지 않고 그리지 않는다.
    if (!rows && screen->cursor_visible && screen->cursor_blink_visible && screen->cursor_shape != 4 &&
        screen->cursor_col < screen->width && screen->cursor_row < screen->height) {
        CGFloat cursor_x = screen->cursor_col * metrics->cell_width;
        CGFloat cursor_y = (CGFloat)frame->height - ((CGFloat)screen->cursor_row + 1.0) * metrics->cell_height;
        CGFloat cursor_width = metrics->cell_width * screen->cursor_width;
        CGFloat cursor_height = metrics->cell_height;
#ifdef SOKSAK_DIAGNOSTICS
        if (render_output) {
            render_output->cursor_visible = 1;
            render_output->cursor_x = cursor_x;
            render_output->cursor_y = cursor_y;
            render_output->cursor_width = cursor_width;
            render_output->cursor_height = cursor_height;
            render_output->cursor_border = 0;
            render_output->cursor_blend_difference = screen->cursor_shape == 0;
            memcpy(render_output->cursor_color, screen->default_cursor, sizeof(render_output->cursor_color));
            if (screen->cursor_shape == 1) {
                render_output->cursor_y = cursor_y;
                render_output->cursor_height = 2.0;
            } else if (screen->cursor_shape == 2) {
                render_output->cursor_width = 2.0;
            } else if (screen->cursor_shape == 3) {
                render_output->cursor_border = 2.0;
            }
        }
#endif
        CGColorRef cursor_color = CGColorCreateSRGB(
            screen->default_cursor[0] / 255.0,
            screen->default_cursor[1] / 255.0,
            screen->default_cursor[2] / 255.0,
            1.0);
        CGContextSetFillColorWithColor(ctx, cursor_color);
        if (screen->cursor_shape == 1) {
            // IOSurface 픽셀 행은 CoreGraphics 좌표계와 반대이므로 메모리 하단에 밑줄을 둔다.
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y,
                                              cursor_width, 2.0));
        } else if (screen->cursor_shape == 2) {
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y, 2.0, cursor_height));
        } else if (screen->cursor_shape == 0) {
            // 블록은 포커스와 관계없이 칸을 채운다. 포커스 없는 모양은 상위 서비스가 이미 골랐다(solid 는 블록).
            CGContextSaveGState(ctx);
            CGContextSetBlendMode(ctx, kCGBlendModeDifference);
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y, cursor_width, cursor_height));
            CGContextRestoreGState(ctx);
        } else {
            CGFloat edge = 2.0;
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y, cursor_width, edge));
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y + cursor_height - edge,
                                              cursor_width, edge));
            CGContextFillRect(ctx, CGRectMake(cursor_x, cursor_y + edge, edge,
                                              cursor_height - 2.0 * edge));
            CGContextFillRect(ctx, CGRectMake(cursor_x + cursor_width - edge, cursor_y + edge,
                                              edge, cursor_height - 2.0 * edge));
        }
        CGColorRelease(cursor_color);
    }
#ifdef SOKSAK_DIAGNOSTICS
    else if (render_output) {
        render_output->cursor_visible = 0;
        render_output->cursor_blend_difference = 0;
        render_output->cursor_x = render_output->cursor_y = 0;
        render_output->cursor_width = render_output->cursor_height = 0;
        render_output->cursor_border = 0;
    }
#endif

    CFRelease(font);
    CGContextRelease(ctx);
    CGColorRelease(default_fg_color);
    CGColorRelease(default_bg_color);

    IOSurfaceUnlock(surface, 0, NULL);

    return 0;
}

// Composites the visible inline images into the overlay surface: it copies the strip pixels of each image rectangle into
// the overlay, then draws every image in order with the CGContextDrawImage call of the CoreGraphics frame, and appends the
// rectangle of each image, limited to the frame, to rects. An image that does not decode returns -1.
static int frame_draw_overlay(Frame *frame, InlineImageRaster *images, uint32_t image_count, NSMutableData *rects) {
    NSMutableArray *decoded = [NSMutableArray array];
    NSMutableData *sizes = [NSMutableData data];
    for (uint32_t i = 0; i < image_count; i++) {
        InlineImageRaster *raster = &images[i];
        if (!raster->visible || inline_image_outside(frame, raster)) continue;
        CGFloat size[2] = {0, 0};
        CGImageRef image = inline_image_decode(raster, &size[0], &size[1]);
        if (!image) return -1;
        // Above the frame: no row of the image is on the frame.
        if (raster->y + ceil(size[1]) <= 0) {
            CGImageRelease(image);
            continue;
        }
        [decoded addObject:(__bridge_transfer id)image];
        CGFloat placed[3] = {size[0], size[1], raster->y};
        [sizes appendBytes:placed length:sizeof(placed)];
        ComposeRect item = {
            .left = raster->x,
            .top = (uint32_t)MAX(0, raster->y),
            .right = (uint32_t)MIN((CGFloat)frame->width, raster->x + ceil(size[0])),
            .bottom = (uint32_t)MIN((CGFloat)frame->height, (CGFloat)raster->y + ceil(size[1])),
            .kind = COMPOSE_OVERLAY,
        };
        [rects appendBytes:&item length:sizeof(item)];
    }
    if (decoded.count == 0) return 0;
    if (IOSurfaceLock(frame->strips, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) return -1;
    if (IOSurfaceLock(frame->overlay, 0, NULL) != kIOReturnSuccess) {
        IOSurfaceUnlock(frame->strips, kIOSurfaceLockReadOnly, NULL);
        return -1;
    }
    const uint8_t *strip_base = IOSurfaceGetBaseAddress(frame->strips);
    uint8_t *overlay_base = IOSurfaceGetBaseAddress(frame->overlay);
    size_t strip_row = IOSurfaceGetBytesPerRow(frame->strips);
    size_t overlay_row = IOSurfaceGetBytesPerRow(frame->overlay);
    for (NSUInteger i = 0; i < decoded.count; i++) {
        const ComposeRect *rect = (const ComposeRect *)rects.bytes + i;
        for (uint32_t y = rect->top; y < rect->bottom; y++) {
            memcpy(overlay_base + y * overlay_row + rect->left * 4, strip_base + y * strip_row + rect->left * 4,
                   (rect->right - rect->left) * 4);
        }
    }
    IOSurfaceUnlock(frame->strips, kIOSurfaceLockReadOnly, NULL);
    CGColorSpaceRef color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef ctx = color_space ? CGBitmapContextCreateWithData(overlay_base, frame->width, frame->height, 8, overlay_row, color_space,
                                                                   kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little, NULL, NULL)
                                   : NULL;
    if (color_space) CGColorSpaceRelease(color_space);
    if (!ctx) {
        IOSurfaceUnlock(frame->overlay, 0, NULL);
        return -1;
    }
    for (NSUInteger i = 0; i < decoded.count; i++) {
        const CGFloat *placed = (const CGFloat *)sizes.bytes + i * 3;
        const ComposeRect *rect = (const ComposeRect *)rects.bytes + i;
        CGContextDrawImage(ctx, CGRectMake(rect->left, (CGFloat)frame->height - placed[2] - placed[1], placed[0], placed[1]),
                           (__bridge CGImageRef)decoded[i]);
    }
    CGContextRelease(ctx);
    IOSurfaceUnlock(frame->overlay, 0, NULL);
    return 0;
}

static void serialize_append(NSMutableData *data, const void *bytes, size_t length) {
    [data appendBytes:bytes length:length];
}

// Copies the strips into the target surface and applies the rectangles. Returns -1 when a surface does not lock.
static int frame_compose(Frame *frame, NSData *rects) {
    if (IOSurfaceLock(frame->strips, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) return -1;
    if (IOSurfaceLock(frame->overlay, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) {
        IOSurfaceUnlock(frame->strips, kIOSurfaceLockReadOnly, NULL);
        return -1;
    }
    if (IOSurfaceLock(frame->surface, 0, NULL) != kIOReturnSuccess) {
        IOSurfaceUnlock(frame->overlay, kIOSurfaceLockReadOnly, NULL);
        IOSurfaceUnlock(frame->strips, kIOSurfaceLockReadOnly, NULL);
        return -1;
    }
    const uint8_t *strip_base = IOSurfaceGetBaseAddress(frame->strips);
    const uint8_t *overlay_base = IOSurfaceGetBaseAddress(frame->overlay);
    uint8_t *target_base = IOSurfaceGetBaseAddress(frame->surface);
    size_t strip_row = IOSurfaceGetBytesPerRow(frame->strips);
    size_t overlay_row = IOSurfaceGetBytesPerRow(frame->overlay);
    size_t target_row = IOSurfaceGetBytesPerRow(frame->surface);
    for (uint32_t y = 0; y < frame->height; y++) {
        memcpy(target_base + y * target_row, strip_base + y * strip_row, (size_t)frame->width * 4);
    }
    const ComposeRect *items = rects.bytes;
    for (NSUInteger i = 0; i < rects.length / sizeof(ComposeRect); i++) {
        const ComposeRect *rect = &items[i];
        for (uint32_t y = rect->top; y < rect->bottom; y++) {
            uint8_t *row = target_base + y * target_row;
            if (rect->kind == COMPOSE_OVERLAY) {
                memcpy(row + rect->left * 4, overlay_base + y * overlay_row + rect->left * 4, (rect->right - rect->left) * 4);
                continue;
            }
            for (uint32_t x = rect->left; x < rect->right; x++) {
                uint8_t *pixel = row + x * 4;
                for (int channel = 0; channel < 3; channel++) {
                    pixel[channel] = rect->kind == COMPOSE_FILL ? rect->bgr[channel] : 255 - pixel[channel];
                }
                pixel[3] = 255;
            }
        }
    }
    IOSurfaceUnlock(frame->surface, 0, NULL);
    IOSurfaceUnlock(frame->overlay, kIOSurfaceLockReadOnly, NULL);
    IOSurfaceUnlock(frame->strips, kIOSurfaceLockReadOnly, NULL);
    return 0;
}

// Draws the screen from strips. The CoreGraphics cell loop draws the rows whose cells differ from the cells their strip was
// drawn with into the strip surface; frame_compose copies the strips into the target, which also fills the margin below the
// rows with the default background, copies the inline images that frame_draw_overlay composited, and draws the cursor with
// the fills of the CoreGraphics frame. It collects no render metadata (-3).
static int frame_draw_strips(Frame *frame, Screen *screen, Metrics *metrics, InlineImageRaster *images, uint32_t image_count,
                          RenderOutput *render_output) {
    if (render_output) return -3;
    // A change of the frame, the font or the default colors draws every strip.
    NSMutableData *configuration = [NSMutableData data];
    serialize_append(configuration, &frame->width, sizeof(frame->width));
    serialize_append(configuration, &frame->height, sizeof(frame->height));
    serialize_append(configuration, &screen->width, sizeof(screen->width));
    serialize_append(configuration, &screen->height, sizeof(screen->height));
    serialize_append(configuration, &metrics->cell_width, sizeof(metrics->cell_width));
    serialize_append(configuration, &metrics->cell_height, sizeof(metrics->cell_height));
    serialize_append(configuration, &metrics->font_size, sizeof(metrics->font_size));
    serialize_append(configuration, &metrics->font, sizeof(metrics->font));
    serialize_append(configuration, screen->default_foreground, 3);
    serialize_append(configuration, screen->default_background, 3);
    NSMutableArray<NSData *> *drawn = (__bridge NSMutableArray<NSData *> *)frame->rows;
    if (![configuration isEqualToData:(__bridge NSData *)frame->configuration]) {
        [drawn removeAllObjects];
        if (frame->configuration) CFRelease(frame->configuration);
        frame->configuration = (__bridge_retained void *)configuration;
    }
    NSMutableArray<NSMutableData *> *cells = [NSMutableArray arrayWithCapacity:screen->height];
    for (uint32_t row = 0; row < screen->height; row++) [cells addObject:[NSMutableData data]];
    for (uint32_t i = 0; i < screen->cell_count; i++) {
        Cell *cell = &screen->cells[i];
        if (cell->row >= screen->height) return -1;
        NSMutableData *data = cells[cell->row];
        serialize_append(data, &cell->col, sizeof(cell->col));
        serialize_append(data, &cell->width, sizeof(cell->width));
        serialize_append(data, &cell->ch_len, sizeof(cell->ch_len));
        if (cell->ch_len > 0 && cell->ch) serialize_append(data, cell->ch, cell->ch_len);
        serialize_append(data, cell->fg, 3);
        serialize_append(data, cell->bg, 3);
        uint8_t flags[4] = {cell->has_fg, cell->has_bg, cell->inverse, cell->underline};
        serialize_append(data, flags, sizeof(flags));
    }
    NSMutableData *selection = [NSMutableData dataWithLength:screen->height];
    uint8_t *rows = selection.mutableBytes;
    uint32_t count = 0;
    for (uint32_t row = 0; row < screen->height; row++) {
        if (row < drawn.count && [drawn[row] isEqualToData:cells[row]]) continue;
        rows[row] = 1;
        count++;
    }
    if (count > 0) {
        int result = frame_draw_cg(frame, frame->strips, screen, metrics, NULL, 0, NULL, rows);
        if (result != 0) {
            // A strip that failed holds unknown pixels, so the next draw draws every strip.
            [drawn removeAllObjects];
            return result;
        }
        [drawn setArray:cells];
    }
    frame->strips_drawn = count;

    // The inline images are composited by the CoreGraphics draw over a copy of the strip pixels under them, so their
    // pixels equal the CoreGraphics frame; frame_compose copies these rectangles over the strips.
    NSMutableData *rects = [NSMutableData data];
    if (image_count > 0) {
        int result = frame_draw_overlay(frame, images, image_count, rects);
        if (result != 0) return result;
    }
    if (screen->cursor_visible && screen->cursor_blink_visible && screen->cursor_shape != 4 &&
        screen->cursor_col < screen->width && screen->cursor_row < screen->height) {
        CGFloat cursor_x = screen->cursor_col * metrics->cell_width;
        CGFloat cursor_y = (CGFloat)frame->height - ((CGFloat)screen->cursor_row + 1.0) * metrics->cell_height;
        CGFloat cursor_width = metrics->cell_width * screen->cursor_width;
        CGFloat cursor_height = metrics->cell_height;
        const uint8_t *color = screen->default_cursor;
        if (screen->cursor_shape == 1) {
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y, cursor_width, 2.0), color, COMPOSE_FILL);
        } else if (screen->cursor_shape == 2) {
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y, 2.0, cursor_height), color, COMPOSE_FILL);
        } else if (screen->cursor_shape == 0) {
            const uint8_t white[3] = {255, 255, 255};
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y, cursor_width, cursor_height), white, COMPOSE_DIFFERENCE);
        } else {
            CGFloat edge = 2.0;
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y, cursor_width, edge), color, COMPOSE_FILL);
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y + cursor_height - edge, cursor_width, edge), color, COMPOSE_FILL);
            compose_append(rects, frame, CGRectMake(cursor_x, cursor_y + edge, edge, cursor_height - 2.0 * edge), color, COMPOSE_FILL);
            compose_append(rects, frame, CGRectMake(cursor_x + cursor_width - edge, cursor_y + edge, edge, cursor_height - 2.0 * edge), color, COMPOSE_FILL);
        }
    }
    int composed = frame_compose(frame, rects);
    if (composed != 0) return composed;
    return 0;
}

#ifdef SOKSAK_DIAGNOSTICS
uint32_t frame_strips_drawn(Frame *frame) {
    return frame->strips_drawn;
}
#endif

// Selects the target of the next draw: the current one when it is not in use, otherwise another such target,
// otherwise a new target. The draw never waits for a surface.
static int frame_select_target(Frame *frame) {
    for (uint32_t step = 0; step < frame->target_count; step++) {
        uint32_t index = (frame->current + step) % frame->target_count;
        FrameTarget *target = &frame->targets[index];
        if (!IOSurfaceIsInUse(target->surface)) {
            frame_use_target(frame, index);
            return 0;
        }
    }
    FrameTarget *targets = realloc(frame->targets, sizeof(FrameTarget) * (frame->target_count + 1));
    if (!targets) return -1;
    frame->targets = targets;
    FrameTarget *target = &frame->targets[frame->target_count];
    memset(target, 0, sizeof(*target));
    target->surface = surface_create(frame->width, frame->height, target->nonce);
    if (!target->surface) return -1;
    target->id = IOSurfaceGetID(target->surface);
    frame->target_count++;
    frame_use_target(frame, frame->target_count - 1);
    return 0;
}

int frame_draw_with_inline_images(Frame *frame, Screen *screen, Metrics *metrics,
                                  InlineImageRaster *images, uint32_t image_count,
                                  RenderOutput *render_output) {
    if (!frame || !screen || !metrics) return -1;
    int selected = frame_select_target(frame);
    if (selected != 0) return selected;
    if (frame->strips) return frame_draw_strips(frame, screen, metrics, images, image_count, render_output);
    return frame_draw_cg(frame, frame->surface, screen, metrics, images, image_count, render_output, NULL);
}

void frame_drop(Frame *frame) {
    if (frame) {
        for (uint32_t i = 0; i < frame->target_count; i++) {
            if (frame->targets[i].surface) CFRelease(frame->targets[i].surface);
        }
        free(frame->targets);
        if (frame->strips) CFRelease(frame->strips);
        if (frame->overlay) CFRelease(frame->overlay);
        if (frame->rows) CFRelease(frame->rows);
        if (frame->configuration) CFRelease(frame->configuration);
        free(frame);
    }
}

FrameFont *frame_font_system_monospace(void) {
    CTFontRef font = CTFontCreateUIFontForLanguage(kCTFontUIFontUserFixedPitch, 12, NULL);
    if (!font) return NULL;
    CTFontDescriptorRef descriptor = CTFontCopyFontDescriptor(font);
    CFRelease(font);
    if (!descriptor) return NULL;
    FrameFont *result = malloc(sizeof(FrameFont));
    if (!result) {
        CFRelease(descriptor);
        return NULL;
    }
    result->descriptor = descriptor;
    return result;
}

FrameFont *frame_font_named(const char *family) {
    if (!family) return NULL;
    NSString *wanted = [NSString stringWithUTF8String:family];
    if (wanted.length == 0) return NULL;
    CTFontDescriptorRef descriptor = CTFontDescriptorCreateWithAttributes(
        (__bridge CFDictionaryRef)@{(id)kCTFontFamilyNameAttribute: wanted});
    if (!descriptor) return NULL;
    // CoreText 는 없는 이름에도 다른 글꼴을 돌려주므로 실제 family 이름을 비교한다.
    CTFontRef probe = CTFontCreateWithFontDescriptor(descriptor, 12, NULL);
    CFStringRef actual = probe ? CTFontCopyFamilyName(probe) : NULL;
    BOOL same = actual && [(__bridge NSString *)actual isEqualToString:wanted];
    if (actual) CFRelease(actual);
    if (probe) CFRelease(probe);
    if (!same) {
        CFRelease(descriptor);
        return NULL;
    }
    FrameFont *font = malloc(sizeof(FrameFont));
    if (!font) {
        CFRelease(descriptor);
        return NULL;
    }
    font->descriptor = descriptor;
    return font;
}

char *frame_font_family(const FrameFont *font) {
    if (!font) return NULL;
    CFStringRef family = CTFontDescriptorCopyAttribute(font->descriptor, kCTFontFamilyNameAttribute);
    if (!family) return NULL;
    char *result = strdup([(__bridge NSString *)family UTF8String]);
    CFRelease(family);
    return result;
}

void frame_font_drop(FrameFont *font) {
    if (!font) return;
    CFRelease(font->descriptor);
    free(font);
}

Metrics frame_metrics(const FrameFont *frame_font, double font_size, double scale) {
    Metrics metrics = {0};
    if (!frame_font) return metrics;

    double scaled_font_size = font_size * scale;
    CTFontRef font = CTFontCreateWithFontDescriptor(frame_font->descriptor, scaled_font_size, NULL);

    // 셀 폭은 'M' 글리프의 advance 다. 문자 코드를 글리프 번호로 바꿔 잰다.
    UniChar char_m = 'M';
    CGGlyph glyph_m = 0;
    if (!CTFontGetGlyphsForCharacters(font, &char_m, &glyph_m, 1)) {
        CFRelease(font);
        return metrics;
    }
    CGSize advances;
    CTFontGetAdvancesForGlyphs(font, kCTFontOrientationHorizontal, &glyph_m, &advances, 1);
    metrics.cell_width = (uint32_t)ceil(advances.width);

    // ascent + descent + leading 으로 높이를 측정한다
    CGFloat ascent = CTFontGetAscent(font);
    CGFloat descent = CTFontGetDescent(font);
    CGFloat leading = CTFontGetLeading(font);
    metrics.ascent = ascent;
    metrics.descent = descent;
    metrics.leading = leading;
    metrics.cell_height = (uint32_t)ceil(ascent + descent + leading);

    // frame_draw 에서 사용할 scale 적용 글꼴 크기를 저장한다
    metrics.font_size = scaled_font_size;
    metrics.font = frame_font;

    CFRelease(font);

    return metrics;
}


// IOSurface 에서 (x, y) 의 pixel 을 읽어 BGRA 값을 out 에 저장한다
// out 은 최소 4바이트여야 한다
// Returns 0 on success, -1 on failure
int frame_pixel(Frame *frame, uint32_t x, uint32_t y, uint8_t *out) {
    if (!frame || !frame->surface || !out) return -1;
    if (x >= frame->width || y >= frame->height) return -1;

    IOSurfaceLock(frame->surface, kIOSurfaceLockReadOnly, NULL);

    void *base_address = IOSurfaceGetBaseAddress(frame->surface);
    if (!base_address) {
        IOSurfaceUnlock(frame->surface, kIOSurfaceLockReadOnly, NULL);
        return -1;
    }

    size_t bytes_per_row = IOSurfaceGetBytesPerRow(frame->surface);
    uint8_t *pixel_ptr = (uint8_t *)base_address + (y * bytes_per_row) + (x * 4);

    // BGRA 형식: B, G, R, A
    out[0] = pixel_ptr[0];  // B
    out[1] = pixel_ptr[1];  // G
    out[2] = pixel_ptr[2];  // R
    out[3] = pixel_ptr[3];  // A

    IOSurfaceUnlock(frame->surface, kIOSurfaceLockReadOnly, NULL);

    return 0;
}
