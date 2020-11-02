package configs

import (
	"os"
	"path"
	"path/filepath"
	"reflect"

	"github.com/spf13/viper"
)

type Config struct {
	viper      *viper.Viper
	configPath string
}

type Configs struct {
	projectConfigs         *Config
	rootConfigs            *Config
	RailwayProductionToken string
	RailwayEnvFilePath     string
}

func IsDevMode() bool {
	environment, exists := os.LookupEnv("RAILWAY_ENV")
	return exists && environment == "develop"
}

func (c *Configs) CreatePathIfNotExist(path string) error {
	dir := filepath.Dir(path)

	if _, err := os.Stat(dir); os.IsNotExist(err) {
		err = os.MkdirAll(dir, os.ModePerm)
		if err != nil {
			return err
		}
	}

	return nil
}

func (c *Configs) unmarshalConfig(config *Config, data interface{}) error {
	err := config.viper.ReadInConfig()
	if err != nil {
		return err
	}
	return config.viper.Unmarshal(&data)
}

func (c *Configs) marshalConfig(config *Config, cfg interface{}) error {
	reflectCfg := reflect.ValueOf(cfg)
	for i := 0; i < reflectCfg.NumField(); i++ {
		k := reflectCfg.Type().Field(i).Name
		v := reflectCfg.Field(i).Interface()
		config.viper.Set(k, v)
	}

	err := c.CreatePathIfNotExist(config.configPath)
	if err != nil {
		return err
	}

	err = config.viper.WriteConfig()

	return err
}

func (c *Configs) matchPath() string {
	path, err := os.Getwd()
	paths, err := c.rootConfigs.viper.Get("projects")
	var match string
	for i := 0; i < len(paths); i++ {
		match, err := filepath.Match(path, paths)
	}
	return match

}

func New() *Configs {
	// DEPRECATED: Project Configs stored in projects (<project>/.railway)
	// Includes projectId, environmentId, etc
	projectDir, err := filepath.Abs("./.railway")
	if err != nil {
		panic(err)
	}
	projectViper := viper.New()

	projectPath := path.Join(projectDir, "./config.json")
	projectViper.SetConfigFile(projectPath)
	projectViper.ReadInConfig()

	if err != nil {
		panic(err)
	}

	projectConfig := &Config{
		viper:      projectViper,
		configPath: projectPath,
	}

	// Root configs stored in root (~/.railway)
	// Includes token, projectId, environmentId (both based on project path), etc
	rootViper := viper.New()
	rootPath := path.Join(os.Getenv("HOME"), ".railway/config.json")
	rootViper.SetConfigFile(rootPath)
	rootViper.ReadInConfig()

	rootConfig := &Config{
		viper:      rootViper,
		configPath: rootPath,
	}

	return &Configs{
		projectConfigs:         projectConfig,
		rootConfigs:            rootConfig,
		RailwayProductionToken: os.Getenv("RAILWAY_TOKEN"),
		RailwayEnvFilePath:     path.Join(projectDir, "env.json"),
	}
}
