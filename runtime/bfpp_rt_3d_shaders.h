#ifndef BFPP_RT_3D_SHADERS_H
#define BFPP_RT_3D_SHADERS_H

/*
 * bfpp_rt_3d_shaders.h — Embedded GLSL 330 core shaders for the BF++ 3D subsystem.
 *
 * All shaders stored as static const char* string literals.
 * Default pipeline: Blinn-Phong with up to 4 shadow-casting lights (PCF optional).
 * Shadow pipeline: depth-only pass for shadow map generation.
 */

/* ── Default vertex shader ──────────────────────────────────── */

static const char *BFPP_VERT_DEFAULT =
    "#version 330 core\n"
    "layout(location = 0) in vec3 aPos;\n"
    "layout(location = 1) in vec3 aNormal;\n"
    "\n"
    "uniform mat4 uModel;\n"
    "uniform mat4 uView;\n"
    "uniform mat4 uProjection;\n"
    "uniform mat4 uLightSpaceMat[4];\n"
    "\n"
    "out vec3 vFragPos;\n"
    "out vec3 vNormal;\n"
    "out vec4 vLightSpacePos[4];\n"
    "\n"
    "void main() {\n"
    "    vec4 worldPos = uModel * vec4(aPos, 1.0);\n"
    "    vFragPos = worldPos.xyz;\n"
    "    vNormal = mat3(transpose(inverse(uModel))) * aNormal;\n"
    "    for (int i = 0; i < 4; i++)\n"
    "        vLightSpacePos[i] = uLightSpaceMat[i] * worldPos;\n"
    "    gl_Position = uProjection * uView * worldPos;\n"
    "}\n";

/* ── Default fragment shader ────────────────────────────────── */

static const char *BFPP_FRAG_DEFAULT =
    "#version 330 core\n"
    "in vec3 vFragPos;\n"
    "in vec3 vNormal;\n"
    "in vec4 vLightSpacePos[4];\n"
    "\n"
    "uniform vec3 uObjectColor;\n"
    "uniform vec3 uViewPos;\n"
    "uniform vec3 uAmbient;\n"
    "\n"
    "struct Light {\n"
    "    vec3 position;\n"
    "    vec3 color;\n"
    "    float intensity;\n"
    "    int castShadow;\n"
    "};\n"
    "uniform Light uLights[4];\n"
    "uniform int uNumLights;\n"
    "uniform int uShadowQuality;\n"
    "uniform sampler2D uShadowMap[4];\n"
    "\n"
    "out vec4 FragColor;\n"
    "\n"
    "float calcShadow(int lightIdx, vec4 lsPos) {\n"
    "    if (uShadowQuality == 0) return 0.0;\n"
    "    vec3 proj = lsPos.xyz / lsPos.w;\n"
    "    proj = proj * 0.5 + 0.5;\n"
    "    if (proj.z > 1.0) return 0.0;\n"
    "    float closestDepth = texture(uShadowMap[lightIdx], proj.xy).r;\n"
    "    float currentDepth = proj.z;\n"
    "    float bias = 0.005;\n"
    "    if (uShadowQuality == 1) {\n"
    "        return currentDepth - bias > closestDepth ? 1.0 : 0.0;\n"
    "    }\n"
    "    float shadow = 0.0;\n"
    "    vec2 texelSize = 1.0 / textureSize(uShadowMap[lightIdx], 0);\n"
    "    for (int x = -1; x <= 1; x++) {\n"
    "        for (int y = -1; y <= 1; y++) {\n"
    "            float d = texture(uShadowMap[lightIdx], proj.xy + vec2(x,y)*texelSize).r;\n"
    "            shadow += currentDepth - bias > d ? 1.0 : 0.0;\n"
    "        }\n"
    "    }\n"
    "    return shadow / 9.0;\n"
    "}\n"
    "\n"
    "void main() {\n"
    "    vec3 norm = normalize(vNormal);\n"
    "    vec3 result = uAmbient * uObjectColor;\n"
    "\n"
    "    for (int i = 0; i < uNumLights; i++) {\n"
    "        vec3 lightDir = normalize(uLights[i].position - vFragPos);\n"
    "        float diff = max(dot(norm, lightDir), 0.0);\n"
    "        vec3 diffuse = diff * uLights[i].color * uLights[i].intensity;\n"
    "        vec3 viewDir = normalize(uViewPos - vFragPos);\n"
    "        vec3 halfDir = normalize(lightDir + viewDir);\n"
    "        float spec = pow(max(dot(norm, halfDir), 0.0), 32.0);\n"
    "        vec3 specular = spec * uLights[i].color * uLights[i].intensity * 0.5;\n"
    "        float shadow = 0.0;\n"
    "        if (uLights[i].castShadow == 1)\n"
    "            shadow = calcShadow(i, vLightSpacePos[i]);\n"
    "        result += (1.0 - shadow) * (diffuse + specular) * uObjectColor;\n"
    "    }\n"
    "    FragColor = vec4(result, 1.0);\n"
    "}\n";

/* ── Shadow depth vertex shader ─────────────────────────────── */

static const char *BFPP_VERT_SHADOW =
    "#version 330 core\n"
    "layout(location = 0) in vec3 aPos;\n"
    "uniform mat4 uLightSpaceMat;\n"
    "uniform mat4 uModel;\n"
    "void main() {\n"
    "    gl_Position = uLightSpaceMat * uModel * vec4(aPos, 1.0);\n"
    "}\n";

/* ── Shadow depth fragment shader ───────────────────────────── */

static const char *BFPP_FRAG_SHADOW =
    "#version 330 core\n"
    "void main() { /* depth written automatically */ }\n";

#endif /* BFPP_RT_3D_SHADERS_H */
