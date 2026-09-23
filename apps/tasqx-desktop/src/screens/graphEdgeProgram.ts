import type { Attributes } from 'graphology-types';
import { EdgeRectangleProgram } from 'sigma/rendering';

/**
 * The inferred-edge style D160 asks for: dashed. Sigma ships no dashed edge
 * program, so this is its own rectangle program with one varying added — how
 * far along the edge a fragment is, in clip space — and a fragment shader that
 * drops every other stretch of it. Picking mode draws the edge solid, so the
 * gaps stay hoverable and clickable.
 *
 * Loaded only after a WebGL check passed: `sigma/rendering` reads
 * `WebGLRenderingContext` as it is imported, which jsdom does not have.
 */

const VERTEX_SHADER = /* glsl */ `
attribute vec4 a_id;
attribute vec4 a_color;
attribute vec2 a_normal;
attribute float a_normalCoef;
attribute vec2 a_positionStart;
attribute vec2 a_positionEnd;
attribute float a_positionCoef;

uniform mat3 u_matrix;
uniform float u_sizeRatio;
uniform float u_zoomRatio;
uniform float u_pixelRatio;
uniform float u_correctionRatio;
uniform float u_minEdgeThickness;
uniform float u_feather;

varying vec4 v_color;
varying vec2 v_normal;
varying float v_thickness;
varying float v_feather;
varying float v_along;

const float bias = 255.0 / 254.0;

void main() {
  float minThickness = u_minEdgeThickness;
  vec2 normal = a_normal * a_normalCoef;
  vec2 position = a_positionStart * (1.0 - a_positionCoef) + a_positionEnd * a_positionCoef;
  float normalLength = length(normal);
  vec2 unitNormal = normal / normalLength;
  float pixelsThickness = max(normalLength, minThickness * u_sizeRatio);
  float webGLThickness = pixelsThickness * u_correctionRatio / u_sizeRatio;
  gl_Position = vec4((u_matrix * vec3(position + unitNormal * webGLThickness, 1)).xy, 0, 1);

  vec2 start = (u_matrix * vec3(a_positionStart, 1)).xy;
  vec2 end = (u_matrix * vec3(a_positionEnd, 1)).xy;
  v_along = a_positionCoef * length(end - start);

  v_thickness = webGLThickness / u_zoomRatio;
  v_normal = unitNormal;
  v_feather = u_feather * u_correctionRatio / u_zoomRatio / u_pixelRatio * 2.0;

  #ifdef PICKING_MODE
  v_color = a_id;
  #else
  v_color = a_color;
  #endif
  v_color.a *= bias;
}
`;

/** One dash plus one gap, in clip-space units (≈ 8 px on a 1,000 px canvas). */
const FRAGMENT_SHADER = /* glsl */ `
precision mediump float;

varying vec4 v_color;
varying vec2 v_normal;
varying float v_thickness;
varying float v_feather;
varying float v_along;

const vec4 transparent = vec4(0.0, 0.0, 0.0, 0.0);
const float period = 0.016;

void main(void) {
  #ifdef PICKING_MODE
  gl_FragColor = v_color;
  #else
  if (mod(v_along, period) > period * 0.55) discard;
  float dist = length(v_normal) * v_thickness;
  float t = smoothstep(v_thickness - v_feather, v_thickness, dist);
  gl_FragColor = mix(v_color, transparent, t);
  #endif
}
`;

/** Sigma's own solid edge, re-exported so the canvas imports both from one place. */
export { EdgeRectangleProgram as EdgeLineProgram };

export class EdgeDashedProgram<
  N extends Attributes = Attributes,
  E extends Attributes = Attributes,
  G extends Attributes = Attributes,
> extends EdgeRectangleProgram<N, E, G> {
  override getDefinition() {
    return { ...super.getDefinition(), VERTEX_SHADER_SOURCE: VERTEX_SHADER, FRAGMENT_SHADER_SOURCE: FRAGMENT_SHADER };
  }
}
