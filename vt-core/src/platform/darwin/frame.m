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

struct Frame {
    IOSurfaceRef surface;
    uint32_t surface_id;
    uint8_t nonce[16];
    uint32_t width;
    uint32_t height;
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

// 주어진 pixel 크기로 frame 을 만든다
Frame* frame_new(uint32_t width_px, uint32_t height_px) {
    Frame *frame = malloc(sizeof(Frame));
    if (!frame) return NULL;

    frame->width = width_px;
    frame->height = height_px;

    // 전역 IOSurface 를 만든다
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

    frame->surface = IOSurfaceCreate((CFDictionaryRef)properties);
    if (!frame->surface) {
        free(frame);
        return NULL;
    }

    frame->surface_id = IOSurfaceGetID(frame->surface);

    // surface 에 sRGB 색 공간을 붙인다
    CGColorSpaceRef srgb_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    if (srgb_space) {
        CFPropertyListRef color_space_plist = CGColorSpaceCopyPropertyList(srgb_space);
        if (color_space_plist) {
            IOSurfaceSetValue(frame->surface, kIOSurfaceColorSpace, color_space_plist);
            CFRelease(color_space_plist);
        }
        CGColorSpaceRelease(srgb_space);
    }

    // 16바이트 nonce 를 생성한다
    arc4random_buf(frame->nonce, 16);

    // nonce 를 surface 의 property 로 설정한다
    NSData *nonce_data = [NSData dataWithBytes:frame->nonce length:16];
    IOSurfaceSetValue(frame->surface, CFSTR("soksak.frame"), (CFDataRef)nonce_data);

    return frame;
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
    return frame_draw_with_inline_images(frame, screen, metrics, NULL, 0);
}

int frame_draw_with_inline_images(Frame *frame, Screen *screen, Metrics *metrics,
                                  InlineImageRaster *images, uint32_t image_count) {
    if (!frame || !screen || !metrics) return -1;

    // surface 를 잠근다
    IOSurfaceLock(frame->surface, 0, NULL);

    // surface 데이터를 가져온다
    void *base_address = IOSurfaceGetBaseAddress(frame->surface);
    if (!base_address) {
        IOSurfaceUnlock(frame->surface, 0, NULL);
        return -1;
    }

    size_t bytes_per_row = IOSurfaceGetBytesPerRow(frame->surface);

    // sRGB 색 공간을 만든다
    CGColorSpaceRef color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    if (!color_space) {
        IOSurfaceUnlock(frame->surface, 0, NULL);
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
        IOSurfaceUnlock(frame->surface, 0, NULL);
        return -1;
    }

    // frame_metrics 에서 사용하는 것과 같은 크기로 글꼴을 설정한다
    CTFontRef font = metrics->font ? CTFontCreateWithFontDescriptor(metrics->font->descriptor, metrics->font_size, NULL) : NULL;
    if (!font) {
        CGContextRelease(ctx);
        IOSurfaceUnlock(frame->surface, 0, NULL);
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
    CGContextFillRect(ctx, CGRectMake(0, 0, (CGFloat)frame->width, (CGFloat)frame->height));

    ColorCache fg_cache = {0};
    ColorCache bg_cache = {0};
    CGFloat descent = CTFontGetDescent(font);
    // 셀을 그린다
    for (uint32_t i = 0; i < screen->cell_count; i++) {
        Cell *cell = &screen->cells[i];

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

        // 기본 배경은 이미 칠했다.
        if (bg_color != default_bg_color) {
            CGContextSetFillColorWithColor(ctx, bg_color);
            CGContextFillRect(ctx, CGRectMake(x, y, metrics->cell_width * cell->width, metrics->cell_height));
        }

        CGGlyph glyph = (cell->ch_len > 0 && cell->width > 0 && cell->ch) ? single_glyph(font, cell->ch, cell->ch_len) : 0;
        if (glyph != 0) {
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
            CTFontDrawGlyphs(font, &glyph, &position, 1, ctx);
        } else if (cell->ch_len > 0 && cell->width > 0) {
            if (!cell->ch) {
                if (fg_cache.color) CGColorRelease(fg_cache.color);
                if (bg_cache.color) CGColorRelease(bg_cache.color);
                CFRelease(font);
                CGContextRelease(ctx);
                CGColorRelease(default_fg_color);
                CGColorRelease(default_bg_color);
                IOSurfaceUnlock(frame->surface, 0, NULL);
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
                IOSurfaceUnlock(frame->surface, 0, NULL);
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
                        // 넓은 글자의 폭이 두 칸보다 좁으면(대체 글꼴의 한글 등) 두 칸 안에서 가로 가운데에 둔다.
                        // 글자 크기는 바꾸지 않는다. 두 칸보다 넓은 글자는 칸의 시작에 둔다.
                        CGFloat offset = 0;
                        if (cell->width == 2) {
                            CGFloat advance = CTLineGetTypographicBounds(line, NULL, NULL, NULL);
                            CGFloat span = metrics->cell_width * 2;
                            if (advance < span) offset = (span - advance) / 2;
                        }
                        CGContextSetTextPosition(ctx, x + offset, y + descent);
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
            CGContextSetFillColorWithColor(ctx, fg_color);
            CGContextFillRect(ctx, CGRectMake(x, MAX(top, y), metrics->cell_width * cell->width, thickness));
        }

    }
    if (fg_cache.color) CGColorRelease(fg_cache.color);
    if (bg_cache.color) CGColorRelease(bg_cache.color);

    for (uint32_t i = 0; i < image_count; i++) {
        InlineImageRaster *raster = &images[i];
        if (!raster->visible) continue;
        if (!raster->data || raster->data_len == 0) {
            CFRelease(font);
            CGContextRelease(ctx);
            CGColorRelease(default_fg_color);
            CGColorRelease(default_bg_color);
            IOSurfaceUnlock(frame->surface, 0, NULL);
            return -1;
        }
        CFDataRef image_data = CFDataCreate(NULL, raster->data, raster->data_len);
        CGImageSourceRef source = image_data ? CGImageSourceCreateWithData(image_data, NULL) : NULL;
        CGImageRef image = source ? CGImageSourceCreateImageAtIndex(source, 0, NULL) : NULL;
        if (image_data) CFRelease(image_data);
        if (source) CFRelease(source);
        if (!image) {
            CFRelease(font);
            CGContextRelease(ctx);
            CGColorRelease(default_fg_color);
            CGColorRelease(default_bg_color);
            IOSurfaceUnlock(frame->surface, 0, NULL);
            return -1;
        }
        CGFloat width = raster->width ? raster->width : (CGFloat)CGImageGetWidth(image);
        CGFloat height = raster->height ? raster->height : (CGFloat)CGImageGetHeight(image);
        if (raster->preserve_aspect_ratio) {
            CGFloat natural_width = (CGFloat)CGImageGetWidth(image);
            CGFloat natural_height = (CGFloat)CGImageGetHeight(image);
            if (raster->width && !raster->height) {
                height = width * natural_height / natural_width;
            } else if (!raster->width && raster->height) {
                width = height * natural_width / natural_height;
            }
        }
        if (width <= 0 || height <= 0 || raster->x >= frame->width || raster->y >= frame->height ||
            width > frame->width - raster->x || height > frame->height - raster->y) {
            CGImageRelease(image);
            CFRelease(font);
            CGContextRelease(ctx);
            CGColorRelease(default_fg_color);
            CGColorRelease(default_bg_color);
            IOSurfaceUnlock(frame->surface, 0, NULL);
            return -1;
        }
        CGContextDrawImage(ctx, CGRectMake(raster->x, frame->height - raster->y - height, width, height), image);
        CGImageRelease(image);
    }

    // 커서 깜박임과 모양은 상위 서비스가 계산한 한 프레임 상태만 그린다.
    // 이 함수는 타이머나 애니메이션을 만들지 않으며, 화면 경계를 벗어난 커서는 버리지 않고 그리지 않는다.
    if (screen->cursor_visible && screen->cursor_blink_visible && screen->cursor_shape != 4 &&
        screen->cursor_col < screen->width && screen->cursor_row < screen->height) {
        CGFloat cursor_x = screen->cursor_col * metrics->cell_width;
        CGFloat cursor_y = (CGFloat)frame->height - ((CGFloat)screen->cursor_row + 1.0) * metrics->cell_height;
        CGFloat cursor_width = metrics->cell_width * screen->cursor_width;
        CGFloat cursor_height = metrics->cell_height;
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

    CFRelease(font);
    CGContextRelease(ctx);
    CGColorRelease(default_fg_color);
    CGColorRelease(default_bg_color);

    IOSurfaceUnlock(frame->surface, 0, NULL);

    return 0;
}

void frame_drop(Frame *frame) {
    if (frame) {
        if (frame->surface) {
            CFRelease(frame->surface);
        }
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
