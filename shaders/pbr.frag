#version 460 core

//#defines

//#import shaders/consts.glsl
//#import shaders/tools/tonemap.glsl
//#import shaders/brdf.glsl

in VsOut
{
    vec2 texCoords;
    vec3 fragPos;
    vec3 normal;
    vec3 tangent;
    vec3 bitangent;
}
vsOut;

out vec4 FragColor;

layout(std140, binding = 1) uniform PbrMaterial
{
    uniform vec4 baseColorFactor;
    uniform vec4 emissiveFactor;
    uniform float metallicFactor;
    uniform float roughnessFactor;
    uniform float normalScale;
    uniform float occlusionStrength;

    uniform float clearcoatIntensityFactor;
    uniform float clearcoatRoughnessFactor;
    uniform float clearcoatNormalScale;

    uniform float anisotropy;
};

#ifdef ALBEDO_MAP
layout(binding = 0) uniform sampler2D abledoTex;
#endif
#ifdef MR_MAP
layout(binding = 1) uniform sampler2D mrTex;
#endif
#ifdef NORMAL_MAP
layout(binding = 2) uniform sampler2D normalTex;
#endif
#ifdef OCCLUSION_MAP
layout(binding = 3) uniform sampler2D occlusionTex;
#endif
#ifdef EMISSIVE_MAP
layout(binding = 4) uniform sampler2D emissiveTex;
#endif

#ifdef CLEARCOAT_INTENSITY_MAP
layout(binding = 5) uniform sampler2D clearcoatIntensityTex;
#endif
#ifdef CLEARCOAT_ROUGHNESS_MAP
layout(binding = 6) uniform sampler2D clearcoatRoughnessTex;
#endif
#ifdef CLEARCOAT_NORMAL_MAP
layout(binding = 7) uniform sampler2D clearcoatNormalTex;
#endif

layout(binding = 8) uniform samplerCube irradianceMap;
layout(binding = 9) uniform samplerCube prefilterMap;
layout(binding = 10) uniform sampler2D brdfLut;

layout(std140, binding = 2) uniform Lighting
{
    uniform vec4 lightPositions[4];
    uniform vec4 lightColors[4];
    uniform vec4 camPos;
    uniform uint lights;
};

layout(std140, binding = 3) uniform Settings
{
    uniform bool clearcoatEnabled;
    uniform bool directLightEnabled;
    uniform bool IBLEnabled;
};

// Parameters that stay same for the whole pixel
struct ShadingParams {
    vec4 albedo;

    vec3 viewDir;
    vec3 normal;
    float NoV;

    float roughness;
    float metalness;
    vec3 f0;

#ifdef CLEARCOAT
    vec3 clearcoatNormal;
    float clearcoatNoV;
    float clearcoatRoughness;
    float clearcoatIntensity;
#endif
};

#if defined(NORMAL_MAP) || defined(CLEARCOAT_NORMAL_MAP)
vec3 getNormalFromMap(sampler2D tex, float scaleNormal, vec3 viewDir)
{
    // https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#_material_normaltextureinfo_scale
    vec3 tangentNormal = normalize((texture(tex, vsOut.texCoords).xyz) * 2.0 - 1.0)
        * vec3(scaleNormal, scaleNormal, 1.0);

    mat3 tbn = mat3(normalize(vsOut.tangent), normalize(vsOut.bitangent), normalize(vsOut.normal));

    return normalize(tbn * tangentNormal);
}
#endif

#ifdef CLEARCOAT
vec3 clearcoatBrdf(ShadingParams sp, out float fresnel, vec3 halfway,
    vec3 lightDir, float VoH)
{
    float clearcoatNoH = max(dot(halfway, sp.clearcoatNormal), 0.0);
    float clearcoatNoL = max(dot(lightDir, sp.clearcoatNormal), 0.0);

    // clearcoat BRDF
    float N = ggxDistribution(clearcoatNoH, sp.clearcoatRoughness);
    float G = smithGeometryShadowing(sp.clearcoatNoV, clearcoatNoL, sp.clearcoatRoughness);
    // Coating IOR is 1.5 (f0 == 0.04) - equivalent to polyurethane
    fresnel = fresnelSchlick(DIELECTRIC_FRESNEL, VoH) * sp.clearcoatIntensity;

    vec3 numerator = N * G * vec3(fresnel);
    float denominator = 4.0 * sp.clearcoatNoV * clearcoatNoL;
    // + 0.0001 to prevent divide by zero
    vec3 specular = numerator / (denominator + 0.0001);

    return specular;
}
#endif

#ifdef ANISOTROPY
void baseSpecularAnisotropic(ShadingParams sp, inout vec3 specular, inout vec3 fresnel, float NoH, float NoL,
    float VoH, vec3 halfway, vec3 lightDir)
{
    float D = anisotropicGgxDistribution(sp.roughness, NoH, halfway, vsOut.tangent, vsOut.bitangent, anisotropy);

    float ToV = dot(vsOut.tangent, sp.viewDir);
    float BoV = dot(vsOut.bitangent, sp.viewDir);
    float ToL = dot(vsOut.tangent, lightDir);
    float BoL = dot(vsOut.bitangent, lightDir);

    // Visibility term (G divided with denominator)
    float V = anisotropicVSmithGgxCorrelated(sp.roughness, sp.NoV, ToV, BoV, ToL, BoL, NoL, anisotropy);

    fresnel = fresnelSchlick(sp.f0, VoH);
    specular = D * V * fresnel;
}
#endif

void baseSpecularIsotropic(ShadingParams sp, inout vec3 specular, inout vec3 fresnel, float NoH,
    float NoL, float VoH)
{
    float D = ggxDistribution(NoH, sp.roughness);
    float G = smithGeometryShadowing(sp.NoV, NoL, sp.roughness);
    fresnel = fresnelSchlick(sp.f0, VoH);

    vec3 numerator = D * G * fresnel;
    float denominator = 4.0 * sp.NoV * NoL;
    // + 0.0001 to prevent divide by zero
    specular = numerator / (denominator + 0.0001);
}

vec3 calculateDirectLighting(ShadingParams sp)
{
    vec3 totalRadiance = vec3(0.);

    for (int i = 0; i < lights; i++) {
        vec3 lightDir = normalize(lightPositions[i].xyz - vsOut.fragPos);
        vec3 halfway = normalize(sp.viewDir + lightDir);
        float VoH = max(dot(halfway, sp.viewDir), 0.0);
        float NoH = max(dot(sp.normal, halfway), 0.0);
        float NoL = max(dot(sp.normal, lightDir), 0.0);

        // TODO: should add attenuation...
        vec3 radiance = lightColors[i].xyz;

        vec3 fresnel;
        vec3 specular;
#ifdef ANISOTROPY
        baseSpecularAnisotropic(sp, specular, fresnel, NoH, NoL, VoH, halfway, lightDir);
#else
        baseSpecularIsotropic(sp, specular, fresnel, NoH, NoL, VoH);
#endif

        // diffuse is 1 - fresnel (the amount of reflected light)
        vec3 kd = vec3(1.0) - fresnel;
        // metals have no diffuse, attenuate for in-between materials
        kd *= 1.0 - sp.metalness;

        // Lambertian diffuse
        vec3 diffuse = kd * sp.albedo.rgb / PI;

#ifdef CLEARCOAT
        float clearcoatFresnel;
        vec3 clearcoatColor = clearcoatBrdf(sp, clearcoatFresnel, halfway, lightDir, VoH);

        vec3 brdf;
        if (clearcoatEnabled) {
            // Energy loss due to the clearcoat layer is given by 1 - clearcoatFresnel
            brdf = (diffuse + specular) * (1. - clearcoatFresnel) + clearcoatColor;
        } else {
            brdf = diffuse + specular;
        }
#else
        vec3 brdf = diffuse + specular;
#endif

        totalRadiance += brdf * radiance * NoL;
    }

    return totalRadiance;
}

vec3 calculateIBL(ShadingParams sp)
{
    vec3 F = fresnelSchlickRoughness(sp.NoV, sp.f0, sp.roughness);
    vec3 kD = 1.0 - F;
    kD *= 1.0 - sp.metalness;

    vec3 irradiance = texture(irradianceMap, sp.normal).rgb;
    vec3 iblDiffuse = irradiance * sp.albedo.rgb;

#ifdef ANISOTROPY
    // Taken from: Guy and Agopian, “Physically Based Rendering in Filament.”
    // Based on
    // McAuley: Rendering the World of Far Cry 4.
    vec3 anisotropicDirection = anisotropy >= 0.0 ? vsOut.bitangent : vsOut.tangent;
    vec3 anisotropicTangent = cross(anisotropicDirection, sp.viewDir);
    vec3 anisotropicNormal = cross(anisotropicTangent, anisotropicDirection);
    vec3 bentNormal = normalize(mix(sp.normal, anisotropicNormal, anisotropy));
    vec3 reflectDir = reflect(-sp.viewDir, bentNormal);
#else
    vec3 reflectDir = reflect(-sp.viewDir, sp.normal);
#endif

    const float MAX_REFLECTION_LOD = 6.0;
    vec3 prefilteredLight = textureLod(prefilterMap, reflectDir, sp.roughness * MAX_REFLECTION_LOD).rgb;
    vec2 dfg = texture(brdfLut, vec2(sp.NoV, sp.roughness)).rg;
    vec3 iblSpecular = prefilteredLight * (F * dfg.x + dfg.y);

    vec3 environmentLight = vec3(0.);

#ifdef CLEARCOAT
    if (clearcoatEnabled) {
        vec3 clearcoatReflectDir = reflect(-sp.viewDir, sp.clearcoatNormal);

        // https://google.github.io/filament/Filament.html#lighting/imagebasedlights/clearcoat
        float clearcoatFresnel = fresnelSchlick(DIELECTRIC_FRESNEL, sp.clearcoatNoV) * sp.clearcoatIntensity;

        // Apply clearcoat IBL
        vec3 clearcoatPrefilteredLight = textureLod(prefilterMap, clearcoatReflectDir, sp.clearcoatRoughness * MAX_REFLECTION_LOD).rgb;
        vec2 clearcoatDfg = texture(brdfLut, vec2(sp.clearcoatNoV, sp.clearcoatRoughness)).rg;
        vec3 clearcoatIblSpecular = clearcoatPrefilteredLight * (clearcoatFresnel * clearcoatDfg.x + clearcoatDfg.y);

        // base layer attenuation for energy compensation
        iblDiffuse *= 1.0 - clearcoatFresnel;
        iblSpecular *= 1.0 - clearcoatFresnel;

        environmentLight = (kD * iblDiffuse + iblSpecular) + clearcoatIblSpecular;
    } else {
        environmentLight = (kD * iblDiffuse + iblSpecular);
    }
#else
    environmentLight = (kD * iblDiffuse + iblSpecular);
#endif

#ifdef OCCLUSION_MAP
    environmentLight *= texture(occlusionTex, vsOut.texCoords).x * occlusionStrength;
#endif

    return environmentLight;
}

#ifdef CLEARCOAT
// Formula from:
// https://google.github.io/filament/Filament.html#materialsystem/clearcoatmodel/baselayermodification
// It's derived from Fresnel's formulas
void modifyBaseF0(inout vec3 f0, float clearcoatIntensity)
{
    vec3 sqrtF0 = sqrt(f0);
    vec3 numerator = (1. - 5. * sqrtF0);
    vec3 denom = (5. - sqrtF0);

    vec3 newF0 = (numerator * numerator) / (denom * denom);
    // Only modify base f0 if there's actually coating
    f0 = mix(f0, newF0, clearcoatIntensity);
}
#endif

void initShadingParams(inout ShadingParams sp)
{
    sp.albedo = baseColorFactor;
#ifdef ALBEDO_MAP
    sp.albedo *= texture(abledoTex, vsOut.texCoords);
#endif

    sp.albedo.rgb = pow(sp.albedo.rgb, vec3(GAMMA));

    sp.viewDir = normalize(camPos.xyz - vsOut.fragPos);

#ifdef NORMAL_MAP
    sp.normal = getNormalFromMap(normalTex, normalScale, sp.viewDir);
#else
    sp.normal = normalize(vsOut.normal);
#endif

    sp.NoV = max(dot(sp.normal, sp.viewDir), 0.0);

    // Disney roughness remapping
    float perceptualRoughness = roughnessFactor;
    sp.metalness = metallicFactor;
#ifdef MR_MAP
    perceptualRoughness *= texture(mrTex, vsOut.texCoords).g;
    sp.metalness *= texture(mrTex, vsOut.texCoords).b;
#endif

    // Prevent division by 0
    sp.roughness = perceptualRoughness * perceptualRoughness;
    sp.roughness = clamp(sp.roughness, ROUGHNESS_MIN, 1.0);

    sp.f0 = vec3(DIELECTRIC_FRESNEL);
    sp.f0 = mix(sp.f0, sp.albedo.rgb, sp.metalness);

#ifdef CLEARCOAT
    sp.clearcoatRoughness = clearcoatRoughnessFactor;

#ifdef CLEARCOAT_ROUGHNESS_MAP
    // For some reason the roughness is read from the *green* channel
    sp.clearcoatRoughness *= texture(clearcoatRoughnessTex, vsOut.texCoords).g;
#endif
    sp.clearcoatRoughness = sp.clearcoatRoughness * sp.clearcoatRoughness;
    // Prevent division by 0
    sp.clearcoatRoughness = clamp(sp.clearcoatRoughness, ROUGHNESS_MIN, 1.0);

    sp.clearcoatIntensity = clearcoatIntensityFactor;
#ifdef CLEARCOAT_INTENSITY_MAP
    sp.clearcoatIntensity *= texture(clearcoatIntensityTex, vsOut.texCoords).r;
#endif

    if (clearcoatEnabled) {
        modifyBaseF0(sp.f0, sp.clearcoatIntensity);
    }

#ifdef CLEARCOAT_NORMAL_MAP
    sp.clearcoatNormal = getNormalFromMap(clearcoatNormalTex, clearcoatNormalScale, sp.viewDir);
#else
    // https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Khronos/KHR_materials_clearcoat/README.md
    // If clearcoatNormalTexture is not given, no normal mapping is applied to the clear coat layer,
    // even if normal mapping is applied to the base material.
    sp.clearcoatNormal = normalize(vsOut.normal);
#endif
    sp.clearcoatNoV = max(dot(sp.clearcoatNormal, sp.viewDir), 0.0);

#endif
}

void main()
{
    ShadingParams sp;
    initShadingParams(sp);

    vec3 color = vec3(0.);

    if (IBLEnabled) {
        color += calculateIBL(sp);
    }

    if (directLightEnabled) {
        color += calculateDirectLighting(sp);
    }

#ifdef EMISSIVE_MAP
    vec4 emissive = texture(emissiveTex, vsOut.texCoords);
    emissive.rgb = pow(emissive.rgb, vec3(GAMMA));
    color += emissive.rgb * emissiveFactor.xyz;
#endif

    tonemap(color);

    // gamma correction
    color = pow(color, vec3(1.0 / GAMMA));

    FragColor = vec4(color, sp.albedo.a);
}
