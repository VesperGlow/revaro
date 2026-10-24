// Adapted from Kirikiroid2 Web 2028cbcc; see SOURCE.md and ENGINE-LICENSE.txt.
// Fix WebGL shader precision mismatch on mobile:
        // cocos2d-x injects uniforms (mat4/mat3) without explicit precision into both
        // vertex (highp default) and fragment (mediump default) shaders, causing link failure.
        (function() {
            var typePattern = /uniform\s+(?!highp\s|mediump\s|lowp\s)(mat[234]|vec[234]|float|int)\s/g;
            function fixPrecision(shader, source) {
                return source.replace(typePattern, 'uniform highp $1 ');
            }
            var proto = WebGLRenderingContext.prototype;
            var origSS = proto.shaderSource;
            proto.shaderSource = function(shader, source) { return origSS.call(this, shader, fixPrecision(shader, source)); };
            if (typeof WebGL2RenderingContext !== 'undefined') {
                var proto2 = WebGL2RenderingContext.prototype;
                var origSS2 = proto2.shaderSource;
                proto2.shaderSource = function(shader, source) { return origSS2.call(this, shader, fixPrecision(shader, source)); };
            }
        })();
